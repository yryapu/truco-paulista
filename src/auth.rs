//! Cadastro, sessão e isolamento entre jogadores.
//!
//! O requisito "dados de cada jogador isolados dos outros" sai de uma regra única e sem
//! exceção: **toda rota autenticada deriva o `jogador_id` do cookie de sessão, nunca de um
//! campo do pedido.** Não existe rota que aceite `jogador_id` do cliente. Se existisse, o
//! isolamento dependeria de cada handler lembrar de conferir; assim, depende do extractor.
//!
//! Decisão D-10: token opaco em tabela, não JWT.

use crate::db::agora;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::Argon2;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum_extra::extract::cookie::{Cookie, SameSite};
use base64::Engine;
use sqlx::SqlitePool;

pub const COOKIE_SESSAO: &str = "truco_sessao";
/// Validade da sessão. Curta o bastante para limitar roubo de cookie, longa o bastante para
/// uma noite de truco não expirar no meio.
pub const HORAS_DE_SESSAO: i64 = 24;

/// Regras de cadastro. "Menos de um minuto" é o requisito, então: apelido e senha, e nada
/// mais. Sem e-mail, sem confirmação, sem captcha.
pub const APELIDO_MIN: usize = 3;
pub const APELIDO_MAX: usize = 20;
pub const SENHA_MIN: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ErroAuth {
    #[error("o apelido precisa ter de 3 a 20 caracteres, so letras, numeros, - e _")]
    ApelidoInvalido,
    #[error("a senha precisa ter ao menos 6 caracteres")]
    SenhaCurta,
    #[error("esse apelido ja esta em uso")]
    ApelidoEmUso,
    #[error("apelido ou senha incorretos")]
    Credenciais,
    #[error("faca login para continuar")]
    SemSessao,
}

pub fn validar_apelido(a: &str) -> Result<(), ErroAuth> {
    let n = a.chars().count();
    if !(APELIDO_MIN..=APELIDO_MAX).contains(&n) {
        return Err(ErroAuth::ApelidoInvalido);
    }
    if !a
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ErroAuth::ApelidoInvalido);
    }
    Ok(())
}

fn hash_da_senha(senha: &str) -> anyhow::Result<String> {
    // password-hash 0.6 gera o sal (16 bytes, do getrandom) dentro de `hash_password`; nao ha
    // mais `SaltString::generate` para eu errar. A anotacao de tipo e necessaria porque
    // `PasswordHasher` e generico sobre o hash de saida.
    let hash: PasswordHash = Argon2::default()
        .hash_password(senha.as_bytes())
        .map_err(|e| anyhow::anyhow!("argon2: {e}"))?;
    Ok(hash.to_string())
}

fn senha_confere(senha: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(h) => Argon2::default()
            .verify_password(senha.as_bytes(), &h)
            .is_ok(),
        Err(_) => false,
    }
}

/// Jogador identificado pelo cookie. A existência deste tipo num handler **é** a prova de
/// que o pedido está autenticado — não há como construí-lo sem passar pelo extractor.
#[derive(Debug, Clone)]
pub struct Jogador {
    pub id: i64,
    pub apelido: String,
}

/// 32 bytes de entropia do sistema operacional, em base64url. 256 bits não se adivinha.
fn novo_token() -> String {
    let bytes: [u8; 32] = rand::random();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub async fn cadastrar(pool: &SqlitePool, apelido: &str, senha: &str) -> anyhow::Result<i64> {
    validar_apelido(apelido)?;
    if senha.chars().count() < SENHA_MIN {
        return Err(ErroAuth::SenhaCurta.into());
    }
    let hash = hash_da_senha(senha)?;
    let mut tx = pool.begin().await?;
    // O UNIQUE COLLATE NOCASE é quem decide a corrida entre dois cadastros simultâneos do
    // mesmo apelido; conferir antes com SELECT seria uma checagem com janela de corrida.
    let r = sqlx::query("INSERT INTO jogador (apelido, senha_hash, criado_em) VALUES (?, ?, ?)")
        .bind(apelido)
        .bind(&hash)
        .bind(agora())
        .execute(&mut *tx)
        .await;
    let id = match r {
        Ok(r) => r.last_insert_rowid(),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(ErroAuth::ApelidoEmUso.into())
        }
        Err(e) => return Err(e.into()),
    };
    // As 1000 moedas iniciais são o primeiro lançamento do ledger, na MESMA transação do
    // cadastro: nunca existe jogador sem saldo inicial.
    sqlx::query(
        "INSERT INTO lancamento (jogador_id, delta, motivo, criado_em) VALUES (?, ?, ?, ?)",
    )
    .bind(id)
    .bind(crate::economia::SALDO_INICIAL)
    .bind("bonus_inicial")
    .bind(agora())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn autenticar(pool: &SqlitePool, apelido: &str, senha: &str) -> anyhow::Result<i64> {
    let linha: Option<(i64, String)> =
        sqlx::query_as("SELECT id, senha_hash FROM jogador WHERE apelido = ?")
            .bind(apelido)
            .fetch_optional(pool)
            .await?;
    // Mensagem única para apelido inexistente e senha errada: dizer qual dos dois falhou
    // entrega a lista de apelidos registrados a quem perguntar.
    let Some((id, hash)) = linha else {
        return Err(ErroAuth::Credenciais.into());
    };
    if !senha_confere(senha, &hash) {
        return Err(ErroAuth::Credenciais.into());
    }
    Ok(id)
}

pub async fn abrir_sessao(pool: &SqlitePool, jogador_id: i64) -> anyhow::Result<String> {
    let token = novo_token();
    let expira = chrono::Utc::now() + chrono::Duration::hours(HORAS_DE_SESSAO);
    sqlx::query("INSERT INTO sessao (token, jogador_id, criada_em, expira_em) VALUES (?, ?, ?, ?)")
        .bind(&token)
        .bind(jogador_id)
        .bind(agora())
        .bind(expira.to_rfc3339())
        .execute(pool)
        .await?;
    Ok(token)
}

pub async fn fechar_sessao(pool: &SqlitePool, token: &str) -> anyhow::Result<()> {
    // Revogação é um DELETE. É isto que o JWT não tem de graça, e foi o argumento de D-10.
    sqlx::query("DELETE FROM sessao WHERE token = ?")
        .bind(token)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn jogador_da_sessao(pool: &SqlitePool, token: &str) -> Option<Jogador> {
    let linha: Option<(i64, String)> = sqlx::query_as(
        "SELECT j.id, j.apelido FROM sessao s JOIN jogador j ON j.id = s.jogador_id
         WHERE s.token = ? AND s.expira_em > ?",
    )
    .bind(token)
    .bind(agora())
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    linha.map(|(id, apelido)| Jogador { id, apelido })
}

/// O cookie de sessão. `HttpOnly` para JavaScript não poder ler (nem o meu próprio front, que
/// não precisa). `SameSite=Strict` para o cookie não subir num pedido disparado de outro
/// site — é o que impede CSRF sem eu ter de carregar token anti-CSRF em cada formulário.
pub fn cookie_de_sessao(token: String) -> Cookie<'static> {
    Cookie::build((COOKIE_SESSAO, token))
        .http_only(true)
        .same_site(SameSite::Strict)
        .path("/")
        .max_age(time_max_age())
        .build()
}

pub fn cookie_de_saida() -> Cookie<'static> {
    Cookie::build((COOKIE_SESSAO, ""))
        .http_only(true)
        .same_site(SameSite::Strict)
        .path("/")
        .max_age(cookie::time::Duration::seconds(0))
        .build()
}

fn time_max_age() -> cookie::time::Duration {
    cookie::time::Duration::hours(HORAS_DE_SESSAO)
}

pub fn token_do_pedido(parts: &Parts) -> Option<String> {
    // O cookie sobe sozinho também no handshake do WebSocket, por ser mesma origem — é
    // por isso que o WS não precisa de um segundo mecanismo de autenticação.
    let bruto = parts
        .headers
        .get(axum::http::header::COOKIE)?
        .to_str()
        .ok()?;
    Cookie::split_parse(bruto.to_string())
        .filter_map(Result::ok)
        .find(|c| c.name() == COOKIE_SESSAO)
        .map(|c| c.value().to_string())
}

impl FromRequestParts<crate::mesa::Estado> for Jogador {
    type Rejection = crate::api::Falha;

    async fn from_request_parts(
        parts: &mut Parts,
        estado: &crate::mesa::Estado,
    ) -> Result<Self, Self::Rejection> {
        let token = token_do_pedido(parts).ok_or(ErroAuth::SemSessao)?;
        jogador_da_sessao(&estado.db, &token)
            .await
            .ok_or(ErroAuth::SemSessao.into())
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn apelido_recusa_o_que_nao_e_identidade_simples() {
        assert!(validar_apelido("ana").is_ok());
        assert!(validar_apelido("ze_do-truco9").is_ok());
        assert!(validar_apelido("já").is_err(), "curto demais");
        assert!(validar_apelido("ab").is_err());
        assert!(validar_apelido(&"a".repeat(21)).is_err());
        assert!(validar_apelido("ana silva").is_err(), "espaco");
        assert!(
            validar_apelido("<script>").is_err(),
            "nada de HTML em apelido"
        );
        assert!(validar_apelido("").is_err());
    }

    #[test]
    fn hash_de_senha_e_argon2id_salgado_e_verificavel() {
        let h1 = hash_da_senha("segredo123").unwrap();
        let h2 = hash_da_senha("segredo123").unwrap();
        assert!(
            h1.starts_with("$argon2id$"),
            "formato PHC do argon2id: {h1}"
        );
        assert_ne!(
            h1, h2,
            "sal diferente por hash, senao hash igual delata senha igual"
        );
        assert!(senha_confere("segredo123", &h1));
        assert!(!senha_confere("segredo124", &h1));
        assert!(!senha_confere("segredo123", "nao-e-um-hash"));
    }

    #[test]
    fn token_de_sessao_tem_256_bits_e_nao_repete() {
        let a = novo_token();
        let b = novo_token();
        assert_ne!(a, b);
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&a)
            .unwrap();
        assert_eq!(bytes.len(), 32, "256 bits de entropia");
    }
}
