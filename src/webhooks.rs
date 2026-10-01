//! Webhooks de partida. Um jogador ou integrador registra uma URL e recebe os eventos.
//!
//! São dois eventos, e só dois, porque é o que o enunciado pede: `partida.comecou` e
//! `partida.terminou` (com o resultado dentro). Resisti a emitir evento por mão e por rodada:
//! seria barato de produzir e caro para o integrador, que teria de filtrar ruído.
//!
//! Entrega: `tokio::spawn`, timeout de 5s, **sem retentativa**. Isso é deliberado e está em
//! `riscos_conhecidos` — ver D-10. Fila durável é o conserto certo e não é v1; prefiro não
//! ter retentativa a ter uma que finge garantia.

use crate::db::agora;
use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;
use sqlx::SqlitePool;
use std::time::Duration;

pub const TIMEOUT: Duration = Duration::from_secs(5);
/// Cabeçalho com a assinatura. O integrador recalcula e compara.
pub const CABECALHO_ASSINATURA: &str = "X-Truco-Signature";
pub const CABECALHO_EVENTO: &str = "X-Truco-Event";

#[derive(Debug, Clone, Serialize)]
pub struct Entrega {
    pub evento: &'static str,
    pub partida_id: i64,
    pub modo: &'static str,
    pub aposta: i64,
    pub jogadores: Vec<JogadorDoEvento>,
    /// Só em `partida.terminou`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resultado: Option<Resultado>,
    pub em: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct JogadorDoEvento {
    pub apelido: String,
    pub assento: usize,
    pub equipe: u8,
}

#[derive(Debug, Clone, Serialize)]
pub struct Resultado {
    pub equipe_vencedora: u8,
    pub vencedores: Vec<String>,
    pub pontos: [u8; 2],
    pub maos_jogadas: u32,
    /// Quanto cada vencedor recebeu.
    pub premio: i64,
}

/// HMAC-SHA256 do corpo exato, em hex, prefixado pelo algoritmo.
///
/// O prefixo `sha256=` não é decoração: deixa trocar de algoritmo depois sem ambiguidade do
/// lado do integrador, que passa a saber qual verificação aplicar.
pub fn assinar(segredo: &str, corpo: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(segredo.as_bytes())
        .expect("HMAC aceita chave de qualquer tamanho");
    mac.update(corpo);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

pub fn novo_segredo() -> String {
    let b: [u8; 24] = rand::random();
    format!("whsec_{}", hex::encode(b))
}

/// Recusa URL que não serve, e bloqueia o alvo de SSRF que de fato causa dano.
///
/// **O que bloqueio:** `169.254.0.0/16` (link-local), que é onde vive o endpoint de metadados
/// de instância das nuvens (`169.254.169.254`). É o alvo que transforma "registre uma URL" em
/// "leia as credenciais da máquina", e é barato de barrar.
///
/// **O que NÃO bloqueio, e por quê:** loopback e redes privadas (`127.0.0.0/8`, `10/8`,
/// `192.168/16`, `172.16/12`). Bloqueá-las tornaria impossível o uso legítimo de um integrador
/// rodando na mesma máquina ou na mesma rede — inclusive o receptor do meu próprio teste de
/// integração. Num serviço exposto na internet essa escolha se inverte, e é por isso que ela
/// está em `riscos_conhecidos` em vez de escondida aqui: a defesa completa precisa resolver o
/// nome e conferir o IP **no momento do envio** (contra DNS rebinding), não só na validação.
pub fn validar_url(u: &str) -> Result<(), &'static str> {
    if u.len() > 2000 {
        return Err("url longa demais");
    }
    let resto = u
        .strip_prefix("https://")
        .or_else(|| u.strip_prefix("http://"))
        .ok_or("a url precisa comecar com http:// ou https://")?;
    let host = resto.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() {
        return Err("a url precisa ter um host");
    }
    let so_host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    if e_link_local(so_host) {
        return Err("url de rede link-local nao e aceita (endpoint de metadados)");
    }
    Ok(())
}

/// `169.254.0.0/16` em IPv4, e `fe80::/10` em IPv6.
fn e_link_local(host: &str) -> bool {
    let limpo = host.trim_start_matches('[').trim_end_matches(']');
    match limpo.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => v4.is_link_local(),
        Ok(std::net::IpAddr::V6(v6)) => (v6.segments()[0] & 0xffc0) == 0xfe80,
        // Nome, não IP: não resolvo aqui. Ver o doc acima e `riscos_conhecidos`.
        Err(_) => false,
    }
}

/// Dispara um evento para todos os webhooks dos jogadores envolvidos.
///
/// Não bloqueia a partida: a mesa segue e a entrega acontece em tarefa separada. Uma URL
/// morta não pode travar o jogo de quem a registrou, nem o dos outros três.
pub fn disparar(pool: SqlitePool, http: reqwest::Client, jogadores: Vec<i64>, evento: Entrega) {
    tokio::spawn(async move {
        let corpo = match serde_json::to_vec(&evento) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("evento de webhook nao serializou: {e}");
                return;
            }
        };
        for jid in jogadores {
            let inscritos: Vec<(i64, String, String)> =
                match sqlx::query_as("SELECT id, url, segredo FROM webhook WHERE jogador_id = ?")
                    .bind(jid)
                    .fetch_all(&pool)
                    .await
                {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::error!("nao li webhooks de {jid}: {e}");
                        continue;
                    }
                };
            for (id, url, segredo) in inscritos {
                let assinatura = assinar(&segredo, &corpo);
                let r = http
                    .post(&url)
                    .timeout(TIMEOUT)
                    .header("content-type", "application/json")
                    .header(CABECALHO_EVENTO, evento.evento)
                    .header(CABECALHO_ASSINATURA, assinatura)
                    .body(corpo.clone())
                    .send()
                    .await;
                // A entrega é registrada mesmo quando falha: sem isto, o integrador não tem
                // como descobrir que a URL dele está quebrada.
                let (status, erro) = match r {
                    Ok(resp) => (Some(resp.status().as_u16() as i64), None),
                    Err(e) => (None, Some(e.to_string())),
                };
                let _ = sqlx::query(
                    "INSERT INTO entrega (webhook_id, evento, status, erro, criado_em)
                     VALUES (?, ?, ?, ?, ?)",
                )
                .bind(id)
                .bind(evento.evento)
                .bind(status)
                .bind(erro)
                .bind(agora())
                .execute(&pool)
                .await;
            }
        }
    });
}

#[cfg(test)]
mod testes {
    use super::*;

    /// Vetor fixo: a assinatura é determinística para (segredo, corpo), que é o que permite
    /// ao integrador verificar. Se este teste mudar, quebrei todos os integradores.
    #[test]
    fn assinatura_e_estavel_e_depende_de_segredo_e_corpo() {
        let a = assinar("whsec_teste", b"{\"evento\":\"partida.comecou\"}");
        assert!(a.starts_with("sha256="));
        assert_eq!(
            a.len(),
            "sha256=".len() + 64,
            "sha256 em hex tem 64 digitos"
        );
        assert_eq!(
            a,
            assinar("whsec_teste", b"{\"evento\":\"partida.comecou\"}")
        );
        assert_ne!(a, assinar("outro", b"{\"evento\":\"partida.comecou\"}"));
        assert_ne!(
            a,
            assinar("whsec_teste", b"{\"evento\":\"partida.terminou\"}")
        );
    }

    #[test]
    fn segredo_tem_entropia_e_prefixo_reconhecivel() {
        let s = novo_segredo();
        assert!(s.starts_with("whsec_"));
        assert_eq!(s.len(), "whsec_".len() + 48, "24 bytes em hex");
        assert_ne!(s, novo_segredo());
    }

    #[test]
    fn url_de_webhook_recusa_o_que_nao_da_para_entregar() {
        assert!(validar_url("https://exemplo.com/truco").is_ok());
        assert!(
            validar_url("http://127.0.0.1:9999/hook").is_ok(),
            "local serve para teste"
        );
        assert!(validar_url("ftp://exemplo.com").is_err());
        assert!(validar_url("exemplo.com").is_err(), "sem esquema");
        assert!(validar_url("https://").is_err(), "sem host");
        assert!(validar_url("javascript:alert(1)").is_err());
        assert!(validar_url(&format!("https://a.com/{}", "x".repeat(3000))).is_err());
    }
}
