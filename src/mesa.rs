//! A mesa: estado vivo da partida, pareamento e a **visão por jogador**.
//!
//! `visao` é a única porta por onde o estado do jogo sai do servidor, e é o lugar onde o
//! requisito "dados de cada jogador isolados dos outros" deixa de ser promessa e passa a ser
//! código: a visão de um assento carrega as cartas **dele**, e dos outros carrega só a
//! contagem. Carta encoberta vira `null`. Nenhum caminho alternativo existe — o WebSocket
//! manda `visao`, não o `Partida`.

use crate::cartas::Carta;
use crate::economia;
use crate::regras::{
    equipe_de, proximo_valor, Acao, Assento, Equipe, Especial, Evento, Fase, Modo, Partida,
};
use crate::webhooks;
use serde::Serialize;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};

/// Pausa entre a mão que acabou e a próxima, para o jogador ver o resultado.
const PAUSA_ENTRE_MAOS: std::time::Duration = std::time::Duration::from_millis(2600);
/// Quantos eventos o log da mesa guarda. É narração, não auditoria.
const LIMITE_DO_LOG: usize = 60;

#[derive(Clone)]
pub struct Estado {
    pub db: SqlitePool,
    pub http: reqwest::Client,
    /// Uma trava para todas as mesas. Pareamento serializado é o que impede dois jogadores de
    /// criarem mesas separadas no mesmo instante procurando o mesmo jogo; com uma trava por
    /// mesa eu precisaria de uma segunda para o pareamento, e seriam duas disciplinas.
    pub mesas: Arc<Mutex<HashMap<i64, Mesa>>>,
}

impl Estado {
    pub fn novo(db: SqlitePool) -> Self {
        Estado {
            db,
            http: reqwest::Client::new(),
            mesas: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Ocupante {
    pub jogador_id: i64,
    pub apelido: String,
}

pub struct Mesa {
    pub id: i64,
    pub modo: Modo,
    pub aposta: i64,
    pub assentos: Vec<Ocupante>,
    /// `None` enquanto a mesa não enche.
    pub partida: Option<Partida>,
    pub log: Vec<Evento>,
    pub tx: broadcast::Sender<u64>,
    pub versao: u64,
    /// Liquidada: o prêmio já foi pago. Guarda contra pagar duas vezes.
    pub liquidada: bool,
}

impl Mesa {
    fn cheia(&self) -> bool {
        self.assentos.len() >= self.modo.jogadores()
    }

    fn assento_de(&self, jogador_id: i64) -> Option<Assento> {
        self.assentos
            .iter()
            .position(|o| o.jogador_id == jogador_id)
    }

    fn avisar(&mut self) {
        self.versao += 1;
        // Erro aqui só significa "ninguém escutando", que é normal numa mesa sem socket.
        let _ = self.tx.send(self.versao);
    }

    fn registrar(&mut self, evs: Vec<Evento>) {
        self.log.extend(evs);
        if self.log.len() > LIMITE_DO_LOG {
            let corte = self.log.len() - LIMITE_DO_LOG;
            self.log.drain(..corte);
        }
    }

    /// A visão que o assento `jogador_id` tem direito de ver. Ver o doc do módulo.
    pub fn visao(&self, jogador_id: i64) -> Visao {
        let meu = self.assento_de(jogador_id);
        let jogadores: Vec<JogadorNaMesa> = self
            .assentos
            .iter()
            .enumerate()
            .map(|(a, o)| JogadorNaMesa {
                assento: a,
                apelido: o.apelido.clone(),
                equipe: equipe_de(a),
                cartas: self
                    .partida
                    .as_ref()
                    .map(|p| p.mao.cartas[a].len())
                    .unwrap_or(0),
            })
            .collect();

        let mut v = Visao {
            tipo: "estado",
            mesa: self.id,
            modo: self.modo,
            aposta: self.aposta,
            seu_assento: meu,
            sua_equipe: meu.map(equipe_de),
            jogadores,
            aguardando: self.partida.is_none(),
            faltam: self.modo.jogadores().saturating_sub(self.assentos.len()),
            pontos: [0, 0],
            vira: None,
            manilha: None,
            sua_mao: Vec::new(),
            maos_visiveis: HashMap::new(),
            na_mesa: Vec::new(),
            rodadas: Vec::new(),
            valor: 0,
            especial: None,
            fase: None,
            pode_pedir: false,
            pode_encobrir: false,
            proposta: None,
            numero_da_mao: 0,
            vencedora: None,
            log: self.log.clone(),
        };

        let (Some(p), Some(meu)) = (self.partida.as_ref(), meu) else {
            return v;
        };
        v.pontos = p.pontos;
        v.vira = Some(p.mao.vira);
        v.manilha = Some(p.mao.vira.valor.proximo().rotulo());
        v.sua_mao = p.mao.cartas[meu].clone();
        v.rodadas = p.mao.rodadas.clone();
        v.valor = p.mao.valor;
        v.especial = Some(p.mao.especial);
        v.fase = Some(p.mao.fase);
        v.numero_da_mao = p.numero_da_mao;
        v.vencedora = p.vencedora;

        // R-10: a mão do parceiro, e só durante a decisão da mão de onze. `pode_ver` é a
        // autoridade; aqui só se obedece a ela.
        for outro in 0..p.jogadores() {
            if outro != meu && p.pode_ver(meu, outro) {
                v.maos_visiveis.insert(outro, p.mao.cartas[outro].clone());
            }
        }

        // R-12: a carta encoberta não revela o caractere nem depois de jogada.
        v.na_mesa = p
            .mao
            .mesa
            .iter()
            .map(|j| JogadaPublica {
                assento: j.assento,
                carta: (!j.encoberta).then_some(j.carta),
                encoberta: j.encoberta,
            })
            .collect();

        if let Fase::Jogando { vez } = p.mao.fase {
            if vez == meu {
                // R-07 + R-10: as mesmas condições que `Partida::pedir` cobra. Botão desligado
                // no cliente é cortesia; a recusa de verdade está no motor.
                v.pode_pedir = p.mao.especial == Especial::Normal
                    && p.mao.bloqueada != Some(equipe_de(meu))
                    && proximo_valor(p.mao.valor).is_some();
                // R-12: proibido encobrir na primeira rodada.
                v.pode_encobrir = !p.mao.rodadas.is_empty();
            }
        }
        if let Fase::Respondendo(a) = p.mao.fase {
            v.proposta = Some(a.proposto);
        }
        v
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct JogadorNaMesa {
    pub assento: Assento,
    pub apelido: String,
    pub equipe: Equipe,
    /// Quantas cartas o jogador tem na mão. Quantas, nunca quais.
    pub cartas: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct JogadaPublica {
    pub assento: Assento,
    /// `None` quando a carta foi jogada encoberta (R-12).
    pub carta: Option<Carta>,
    pub encoberta: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Visao {
    pub tipo: &'static str,
    pub mesa: i64,
    pub modo: Modo,
    pub aposta: i64,
    pub seu_assento: Option<Assento>,
    pub sua_equipe: Option<Equipe>,
    pub jogadores: Vec<JogadorNaMesa>,
    pub aguardando: bool,
    pub faltam: usize,
    pub pontos: [u8; 2],
    pub vira: Option<Carta>,
    /// Rótulo do valor que é manilha nesta mão, para a mesa mostrar "manilha: 6".
    pub manilha: Option<&'static str>,
    pub sua_mao: Vec<Carta>,
    /// Mãos de outros assentos que este jogador tem direito de ver (R-10, mão de onze).
    pub maos_visiveis: HashMap<Assento, Vec<Carta>>,
    pub na_mesa: Vec<JogadaPublica>,
    pub rodadas: Vec<Option<Equipe>>,
    pub valor: u8,
    pub especial: Option<Especial>,
    pub fase: Option<Fase>,
    pub pode_pedir: bool,
    pub pode_encobrir: bool,
    /// Valor proposto quando há pedido na mesa aguardando resposta.
    pub proposta: Option<u8>,
    pub numero_da_mao: u32,
    pub vencedora: Option<Equipe>,
    pub log: Vec<Evento>,
}

#[derive(Debug, thiserror::Error)]
pub enum ErroMesa {
    #[error("mesa nao encontrada")]
    SemMesa,
    #[error("voce nao esta nesta mesa")]
    ForaDaMesa,
    #[error("a partida ainda nao comecou")]
    NaoComecou,
    #[error("{0}")]
    Regra(#[from] crate::regras::Erro),
    #[error("{0}")]
    Outro(String),
}

impl Estado {
    /// Pareamento: senta o jogador numa mesa aberta de mesmo modo e aposta, ou abre uma.
    ///
    /// Roda inteiro sob a trava, inclusive os INSERTs: o pareamento é o ponto onde duas
    /// requisições simultâneas se atropelariam, e serializar é mais simples que reconciliar.
    pub async fn entrar(
        &self,
        jogador_id: i64,
        apelido: &str,
        modo: Modo,
        aposta: i64,
    ) -> anyhow::Result<i64> {
        if aposta < 0 {
            return Err(economia::ErroEconomia::ApostaNegativa.into());
        }
        let saldo = economia::saldo(&self.db, jogador_id).await?;
        if saldo < aposta {
            return Err(economia::ErroEconomia::SaldoInsuficiente.into());
        }

        let mut mesas = self.mesas.lock().await;

        // Já está sentado em algum lugar? Devolve a mesa dele em vez de abrir outra.
        if let Some(id) = mesas
            .values()
            .find(|m| {
                m.assento_de(jogador_id).is_some()
                    && m.partida.as_ref().is_none_or(|p| p.vencedora.is_none())
            })
            .map(|m| m.id)
        {
            return Ok(id);
        }

        let alvo = mesas
            .values()
            .find(|m| m.modo == modo && m.aposta == aposta && m.partida.is_none() && !m.cheia())
            .map(|m| m.id);

        let id = match alvo {
            Some(id) => id,
            None => {
                let r = sqlx::query(
                    "INSERT INTO partida (modo, aposta, estado, criada_em)
                     VALUES (?, ?, 'aguardando', ?)",
                )
                .bind(nome_do_modo(modo))
                .bind(aposta)
                .bind(crate::db::agora())
                .execute(&self.db)
                .await?;
                let id = r.last_insert_rowid();
                let (tx, _) = broadcast::channel(64);
                mesas.insert(
                    id,
                    Mesa {
                        id,
                        modo,
                        aposta,
                        assentos: Vec::new(),
                        partida: None,
                        log: Vec::new(),
                        tx,
                        versao: 0,
                        liquidada: false,
                    },
                );
                id
            }
        };

        let mesa = mesas
            .get_mut(&id)
            .expect("acabou de ser inserida ou encontrada");
        let assento = mesa.assentos.len();
        mesa.assentos.push(Ocupante {
            jogador_id,
            apelido: apelido.to_string(),
        });
        sqlx::query("INSERT INTO participacao (partida_id, jogador_id, assento) VALUES (?, ?, ?)")
            .bind(id)
            .bind(jogador_id)
            .bind(assento as i64)
            .execute(&self.db)
            .await?;

        if mesa.cheia() {
            let jogadores: Vec<i64> = mesa.assentos.iter().map(|o| o.jogador_id).collect();
            let aposta = mesa.aposta;
            // A aposta sai do bolso de todos ou de ninguém. Se falhar, a mesa fica esperando
            // em vez de começar com alguém debitado.
            economia::debitar_apostas(&self.db, id, &jogadores, aposta).await?;
            sqlx::query("UPDATE partida SET estado = 'em_curso' WHERE id = ?")
                .bind(id)
                .execute(&self.db)
                .await?;
            let p = Partida::nova(modo);
            let abertura = p.evento_de_abertura();
            mesa.partida = Some(p);
            mesa.registrar(vec![abertura]);
            let evento = webhooks::Entrega {
                evento: "partida.comecou",
                partida_id: id,
                modo: nome_do_modo(modo),
                aposta,
                jogadores: mesa
                    .assentos
                    .iter()
                    .enumerate()
                    .map(|(a, o)| webhooks::JogadorDoEvento {
                        apelido: o.apelido.clone(),
                        assento: a,
                        equipe: equipe_de(a),
                    })
                    .collect(),
                resultado: None,
                em: crate::db::agora(),
            };
            webhooks::disparar(self.db.clone(), self.http.clone(), jogadores, evento);
        }
        mesa.avisar();
        Ok(id)
    }

    /// Sai de uma mesa que ainda não começou, com estorno da aposta.
    pub async fn sair(&self, jogador_id: i64) -> anyhow::Result<()> {
        let mut mesas = self.mesas.lock().await;
        let Some(id) = mesas
            .values()
            .find(|m| m.assento_de(jogador_id).is_some() && m.partida.is_none())
            .map(|m| m.id)
        else {
            return Ok(()); // nada para sair, ou a partida já começou
        };
        let mesa = mesas.get_mut(&id).expect("encontrada acima");
        mesa.assentos.retain(|o| o.jogador_id != jogador_id);
        sqlx::query("DELETE FROM participacao WHERE partida_id = ? AND jogador_id = ?")
            .bind(id)
            .bind(jogador_id)
            .execute(&self.db)
            .await?;
        let vazia = mesa.assentos.is_empty();
        mesa.avisar();
        if vazia {
            mesas.remove(&id);
            sqlx::query("DELETE FROM partida WHERE id = ? AND estado = 'aguardando'")
                .bind(id)
                .execute(&self.db)
                .await?;
        }
        Ok(())
    }

    pub async fn receptor(&self, mesa_id: i64) -> Option<broadcast::Receiver<u64>> {
        self.mesas
            .lock()
            .await
            .get(&mesa_id)
            .map(|m| m.tx.subscribe())
    }

    pub async fn visao(&self, mesa_id: i64, jogador_id: i64) -> Option<Visao> {
        let mesas = self.mesas.lock().await;
        let mesa = mesas.get(&mesa_id)?;
        mesa.assento_de(jogador_id)?;
        Some(mesa.visao(jogador_id))
    }

    /// Aplica uma ação de jogo. O assento vem de quem o jogador **é**, não do que ele manda:
    /// é a segunda metade do isolamento. Um cliente não consegue jogar pela cadeira alheia
    /// porque não existe campo de assento no protocolo.
    pub async fn aplicar(&self, mesa_id: i64, jogador_id: i64, acao: Acao) -> Result<(), ErroMesa> {
        let (acabou, encerrou_mao) = {
            let mut mesas = self.mesas.lock().await;
            let mesa = mesas.get_mut(&mesa_id).ok_or(ErroMesa::SemMesa)?;
            let assento = mesa.assento_de(jogador_id).ok_or(ErroMesa::ForaDaMesa)?;
            let p = mesa.partida.as_mut().ok_or(ErroMesa::NaoComecou)?;
            // O motor valida tudo antes de mutar, entao um erro aqui sai por `?` sem deixar
            // a mesa em estado parcial.
            let evs = p.aplicar(assento, acao)?;
            let acabou = p.vencedora.is_some();
            let encerrou_mao = matches!(p.mao.fase, Fase::Encerrada { .. });
            mesa.registrar(evs);
            mesa.avisar();
            (acabou, encerrou_mao)
        };

        if acabou {
            self.liquidar(mesa_id).await;
        } else if encerrou_mao {
            self.agendar_proxima_mao(mesa_id);
        }
        Ok(())
    }

    /// Depois de uma mão encerrada, a próxima começa sozinha — com pausa para o jogador ver
    /// o resultado. Tarefa separada: a mesa não pode ficar travada esperando um relógio.
    fn agendar_proxima_mao(&self, mesa_id: i64) {
        let mesas = self.mesas.clone();
        let eu = self.clone();
        tokio::spawn(async move {
            // Só agenda se de fato a mão acabou; chamadas normais caem fora aqui.
            {
                let m = mesas.lock().await;
                let Some(mesa) = m.get(&mesa_id) else { return };
                let Some(p) = mesa.partida.as_ref() else {
                    return;
                };
                if !matches!(p.mao.fase, Fase::Encerrada { .. }) || p.vencedora.is_some() {
                    return;
                }
            }
            tokio::time::sleep(PAUSA_ENTRE_MAOS).await;
            let acabou = {
                let mut m = mesas.lock().await;
                let Some(mesa) = m.get_mut(&mesa_id) else {
                    return;
                };
                let Some(p) = mesa.partida.as_mut() else {
                    return;
                };
                match p.proxima_mao() {
                    Some(ev) => {
                        mesa.registrar(vec![ev]);
                        mesa.avisar();
                        false
                    }
                    None => p.vencedora.is_some(),
                }
            };
            if acabou {
                eu.liquidar(mesa_id).await;
            }
        });
    }

    /// Paga o prêmio, grava o resultado e dispara `partida.terminou`. Idempotente pela
    /// bandeira `liquidada`: uma partida não pode pagar duas vezes.
    async fn liquidar(&self, mesa_id: i64) {
        let dados = {
            let mut mesas = self.mesas.lock().await;
            let Some(mesa) = mesas.get_mut(&mesa_id) else {
                return;
            };
            if mesa.liquidada {
                return;
            }
            let Some(p) = mesa.partida.as_ref() else {
                return;
            };
            let Some(eq) = p.vencedora else { return };
            mesa.liquidada = true;
            let vencedores: Vec<i64> = mesa
                .assentos
                .iter()
                .enumerate()
                .filter(|(a, _)| equipe_de(*a) == eq)
                .map(|(_, o)| o.jogador_id)
                .collect();
            Some((
                eq,
                vencedores,
                mesa.assentos
                    .iter()
                    .map(|o| o.jogador_id)
                    .collect::<Vec<_>>(),
                mesa.aposta,
                mesa.modo,
                p.pontos,
                p.numero_da_mao,
                mesa.assentos
                    .iter()
                    .enumerate()
                    .map(|(a, o)| webhooks::JogadorDoEvento {
                        apelido: o.apelido.clone(),
                        assento: a,
                        equipe: equipe_de(a),
                    })
                    .collect::<Vec<_>>(),
                mesa.assentos
                    .iter()
                    .enumerate()
                    .filter(|(a, _)| equipe_de(*a) == eq)
                    .map(|(_, o)| o.apelido.clone())
                    .collect::<Vec<_>>(),
            ))
        };
        let Some((eq, vencedores, todos, aposta, modo, pontos, maos, jogadores, apelidos)) = dados
        else {
            return;
        };

        if let Err(e) = economia::pagar_vencedores(&self.db, mesa_id, &vencedores, aposta).await {
            tracing::error!("premio da partida {mesa_id} nao foi pago: {e}");
        }
        if let Err(e) = sqlx::query(
            "UPDATE partida SET estado = 'encerrada', equipe_vencedora = ?, encerrada_em = ?
             WHERE id = ?",
        )
        .bind(eq as i64)
        .bind(crate::db::agora())
        .bind(mesa_id)
        .execute(&self.db)
        .await
        {
            tracing::error!("resultado da partida {mesa_id} nao foi gravado: {e}");
        }
        webhooks::disparar(
            self.db.clone(),
            self.http.clone(),
            todos,
            webhooks::Entrega {
                evento: "partida.terminou",
                partida_id: mesa_id,
                modo: nome_do_modo(modo),
                aposta,
                jogadores,
                resultado: Some(webhooks::Resultado {
                    equipe_vencedora: eq,
                    vencedores: apelidos,
                    pontos,
                    maos_jogadas: maos,
                    premio: aposta * 2,
                }),
                em: crate::db::agora(),
            },
        );
        if let Some(mesa) = self.mesas.lock().await.get_mut(&mesa_id) {
            mesa.avisar();
        }
    }
}

pub const fn nome_do_modo(m: Modo) -> &'static str {
    match m {
        Modo::UmVsUm => "um_vs_um",
        Modo::DoisVsDois => "dois_vs_dois",
    }
}

pub fn modo_do_nome(s: &str) -> Option<Modo> {
    match s {
        "um_vs_um" | "1x1" => Some(Modo::UmVsUm),
        "dois_vs_dois" | "2x2" => Some(Modo::DoisVsDois),
        _ => None,
    }
}
