//! Saldo interno, ranking e emblemas de reputação.
//!
//! **O saldo não é uma coluna.** É `SUM(delta)` sobre `lancamento`, que só recebe INSERT.
//! Com coluna, um bug de aposta deixa o saldo errado e sem rastro; com ledger, todo saldo é
//! explicável — dá para listar de onde veio cada moeda. Ver `migrations/0001_inicial.sql`.
//!
//! As moedas são só do jogo. Não têm valor fora dele, não se compram e não se transferem
//! entre jogadores: os únicos lançamentos possíveis são o bônus inicial, a aposta e o prêmio.

use crate::db::agora;
use serde::Serialize;
use sqlx::SqlitePool;

/// Todo jogador começa com 1000 moedas, gravadas como o primeiro lançamento do ledger.
pub const SALDO_INICIAL: i64 = 1000;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ErroEconomia {
    #[error("saldo insuficiente para essa aposta")]
    SaldoInsuficiente,
    #[error("a aposta nao pode ser negativa")]
    ApostaNegativa,
}

pub async fn saldo(pool: &SqlitePool, jogador_id: i64) -> anyhow::Result<i64> {
    let (s,): (i64,) =
        sqlx::query_as("SELECT COALESCE(SUM(delta), 0) FROM lancamento WHERE jogador_id = ?")
            .bind(jogador_id)
            .fetch_one(pool)
            .await?;
    Ok(s)
}

#[derive(Debug, Clone, Serialize)]
pub struct Lancamento {
    pub delta: i64,
    pub motivo: String,
    pub partida_id: Option<i64>,
    pub criado_em: String,
}

pub async fn extrato(pool: &SqlitePool, jogador_id: i64) -> anyhow::Result<Vec<Lancamento>> {
    let linhas: Vec<(i64, String, Option<i64>, String)> = sqlx::query_as(
        "SELECT delta, motivo, partida_id, criado_em FROM lancamento
         WHERE jogador_id = ? ORDER BY id DESC LIMIT 50",
    )
    .bind(jogador_id)
    .fetch_all(pool)
    .await?;
    Ok(linhas
        .into_iter()
        .map(|(delta, motivo, partida_id, criado_em)| Lancamento {
            delta,
            motivo,
            partida_id,
            criado_em,
        })
        .collect())
}

/// Debita a aposta de todos os jogadores da mesa, numa transação só.
///
/// Tudo ou nada é o ponto: se o quarto jogador não tem saldo, os três primeiros não podem
/// ficar debitados. O `SELECT` de saldo acontece **dentro** da transação, então o débito e a
/// conferência não têm janela entre si.
pub async fn debitar_apostas(
    pool: &SqlitePool,
    partida_id: i64,
    jogadores: &[i64],
    aposta: i64,
) -> anyhow::Result<()> {
    if aposta < 0 {
        return Err(ErroEconomia::ApostaNegativa.into());
    }
    if aposta == 0 {
        return Ok(()); // mesa amistosa: nada a mover
    }
    let mut tx = pool.begin().await?;
    for id in jogadores {
        let (s,): (i64,) =
            sqlx::query_as("SELECT COALESCE(SUM(delta), 0) FROM lancamento WHERE jogador_id = ?")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        if s < aposta {
            return Err(ErroEconomia::SaldoInsuficiente.into()); // rollback no drop
        }
        sqlx::query(
            "INSERT INTO lancamento (jogador_id, delta, motivo, partida_id, criado_em)
             VALUES (?, ?, 'aposta', ?, ?)",
        )
        .bind(id)
        .bind(-aposta)
        .bind(partida_id)
        .bind(agora())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Paga o bolo aos vencedores. Cada vencedor recebe `2 × aposta`: a sua de volta mais a de
/// um perdedor. Fecha exatamente, em 1x1 e em 2x2, porque as equipes têm o mesmo tamanho.
pub async fn pagar_vencedores(
    pool: &SqlitePool,
    partida_id: i64,
    vencedores: &[i64],
    aposta: i64,
) -> anyhow::Result<()> {
    if aposta == 0 {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    for id in vencedores {
        sqlx::query(
            "INSERT INTO lancamento (jogador_id, delta, motivo, partida_id, criado_em)
             VALUES (?, ?, 'premio', ?, ?)",
        )
        .bind(id)
        .bind(aposta * 2)
        .bind(partida_id)
        .bind(agora())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Devolve a aposta quando a mesa se desfaz antes de começar.
pub async fn estornar(
    pool: &SqlitePool,
    partida_id: i64,
    jogadores: &[i64],
    aposta: i64,
) -> anyhow::Result<()> {
    if aposta == 0 {
        return Ok(());
    }
    for id in jogadores {
        sqlx::query(
            "INSERT INTO lancamento (jogador_id, delta, motivo, partida_id, criado_em)
             VALUES (?, ?, 'estorno', ?, ?)",
        )
        .bind(id)
        .bind(aposta)
        .bind(partida_id)
        .bind(agora())
        .execute(pool)
        .await?;
    }
    Ok(())
}

// ----- Reputação -----
//
// O enunciado pede emblemas "por vitórias e por histórico". São dois eixos de propósito,
// porque medem coisas diferentes e um não substitui o outro: vitórias premiam resultado,
// histórico premia permanência e consistência. Quem ganhou 3 de 3 não é veterano; quem
// jogou 200 e ganhou 40% é.

/// Eixo 1: quantas partidas o jogador venceu.
pub fn emblema_vitorias(vitorias: i64) -> &'static str {
    match vitorias {
        0 => "Entrando na Mesa",
        1..=4 => "Primeira Mão",
        5..=14 => "Trucador",
        15..=39 => "Mão de Ferro",
        _ => "Doutor do Truco",
    }
}

/// Eixo 2: volume de partidas, com a consistência como desempate no mesmo volume.
///
/// A taxa só entra a partir de 10 partidas. Abaixo disso, 2 de 2 é ruído, não reputação —
/// dar "Pé-quente" a quem jogou duas vezes é um emblema que mede sorte.
pub fn emblema_historico(partidas: i64, vitorias: i64) -> &'static str {
    let taxa = if partidas > 0 {
        vitorias as f64 / partidas as f64
    } else {
        0.0
    };
    match partidas {
        0 => "Sem Histórico",
        1..=9 => "Estreante",
        10..=29 if taxa >= 0.6 => "Pé-quente",
        10..=29 => "Rodado",
        _ if taxa >= 0.6 => "Lenda da Mesa",
        _ => "Casca-grossa",
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LinhaDoRanking {
    pub posicao: i64,
    pub apelido: String,
    pub vitorias: i64,
    pub derrotas: i64,
    pub partidas: i64,
    pub saldo: i64,
    pub emblema_vitorias: &'static str,
    pub emblema_historico: &'static str,
}

/// O ranking. Ordena por vitórias, e desempata por **menos** partidas — entre dois jogadores
/// com 10 vitórias, quem precisou de menos partidas jogou melhor.
///
/// A equipe de um jogador numa partida é `assento % 2`, a mesma conta de `regras::equipe_de`.
/// Daí `p.equipe_vencedora = pa.assento % 2` decidir a vitória sem precisar de coluna extra.
pub async fn ranking(pool: &SqlitePool, limite: i64) -> anyhow::Result<Vec<LinhaDoRanking>> {
    let linhas: Vec<(String, i64, i64, i64)> = sqlx::query_as(
        "SELECT j.apelido,
                COALESCE(SUM(CASE WHEN p.estado = 'encerrada'
                                   AND p.equipe_vencedora = (pa.assento % 2)
                             THEN 1 ELSE 0 END), 0) AS vitorias,
                COALESCE(SUM(CASE WHEN p.estado = 'encerrada' THEN 1 ELSE 0 END), 0) AS partidas,
                (SELECT COALESCE(SUM(delta), 0) FROM lancamento l WHERE l.jogador_id = j.id) AS saldo
         FROM jogador j
         LEFT JOIN participacao pa ON pa.jogador_id = j.id
         LEFT JOIN partida p ON p.id = pa.partida_id
         GROUP BY j.id
         ORDER BY vitorias DESC, partidas ASC, j.apelido ASC
         LIMIT ?",
    )
    .bind(limite)
    .fetch_all(pool)
    .await?;
    Ok(linhas
        .into_iter()
        .enumerate()
        .map(|(i, (apelido, vitorias, partidas, saldo))| LinhaDoRanking {
            posicao: i as i64 + 1,
            apelido,
            vitorias,
            derrotas: partidas - vitorias,
            partidas,
            saldo,
            emblema_vitorias: emblema_vitorias(vitorias),
            emblema_historico: emblema_historico(partidas, vitorias),
        })
        .collect())
}

#[derive(Debug, Clone, Serialize)]
pub struct Perfil {
    pub apelido: String,
    pub saldo: i64,
    pub vitorias: i64,
    pub derrotas: i64,
    pub partidas: i64,
    pub emblema_vitorias: &'static str,
    pub emblema_historico: &'static str,
}

pub async fn perfil(pool: &SqlitePool, jogador_id: i64, apelido: &str) -> anyhow::Result<Perfil> {
    let (vitorias, partidas): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(CASE WHEN p.estado = 'encerrada'
                                   AND p.equipe_vencedora = (pa.assento % 2)
                             THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN p.estado = 'encerrada' THEN 1 ELSE 0 END), 0)
         FROM participacao pa JOIN partida p ON p.id = pa.partida_id
         WHERE pa.jogador_id = ?",
    )
    .bind(jogador_id)
    .fetch_one(pool)
    .await?;
    Ok(Perfil {
        apelido: apelido.to_string(),
        saldo: saldo(pool, jogador_id).await?,
        vitorias,
        derrotas: partidas - vitorias,
        partidas,
        emblema_vitorias: emblema_vitorias(vitorias),
        emblema_historico: emblema_historico(partidas, vitorias),
    })
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn os_dois_eixos_de_emblema_medem_coisas_diferentes() {
        assert_eq!(emblema_vitorias(0), "Entrando na Mesa");
        assert_eq!(emblema_vitorias(1), "Primeira Mão");
        assert_eq!(emblema_vitorias(14), "Trucador");
        assert_eq!(emblema_vitorias(15), "Mão de Ferro");
        assert_eq!(emblema_vitorias(1000), "Doutor do Truco");

        assert_eq!(emblema_historico(0, 0), "Sem Histórico");
        assert_eq!(
            emblema_historico(9, 9),
            "Estreante",
            "9 de 9 ainda e amostra pequena"
        );
        assert_eq!(emblema_historico(10, 8), "Pé-quente");
        assert_eq!(emblema_historico(10, 3), "Rodado");
        assert_eq!(emblema_historico(100, 70), "Lenda da Mesa");
        assert_eq!(emblema_historico(100, 20), "Casca-grossa");
    }

    /// A razão de a taxa só valer a partir de 10 partidas: senão o emblema mede sorte.
    #[test]
    fn taxa_de_vitoria_nao_conta_em_amostra_pequena() {
        assert_eq!(emblema_historico(2, 2), "Estreante");
        assert_ne!(emblema_historico(2, 2), "Pé-quente");
    }

    #[test]
    fn o_bolo_fecha_exatamente_nos_dois_modos() {
        for (jogadores, aposta) in [(2i64, 50i64), (4, 50), (2, 1), (4, 999)] {
            let arrecadado = jogadores * aposta;
            let pago = (jogadores / 2) * (aposta * 2);
            assert_eq!(
                arrecadado, pago,
                "moeda criada ou destruida com {jogadores} jogadores"
            );
        }
    }
}
