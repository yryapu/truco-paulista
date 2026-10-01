//! O motor de regras do truco paulista. **Puro**: sem I/O, sem async, sem banco.
//!
//! Tudo que decide quem ganha mora aqui, e só aqui. O servidor é autoritativo sobre as cartas
//! (ver decisão D-11 no repositório de pesquisa) — o cliente nunca avalia força nem conhece
//! carta alheia. Por isso `visao_para` existe: é a única porta por onde o estado sai.
//!
//! IDs de regra (R-nn) e de decisão (D-nn) referem-se a
//! <https://github.com/yryapu/poliorketikos-truco-paulista/blob/main/regras/truco-paulista.md>.

use crate::cartas::{baralho, Carta, Forca, FORCA_ENCOBERTA};
use rand::seq::SliceRandom;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};

pub type Equipe = u8;
pub type Assento = usize;

/// Pontos para vencer a partida (R-09).
pub const PONTOS_PARA_VENCER: u8 = 12;
/// Pontuação a partir da qual a mão é especial (R-10).
pub const PONTOS_MAO_DE_ONZE: u8 = 11;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Modo {
    /// 1x1. Nenhuma das quatro fontes descreve esta variante; ver D-03.
    UmVsUm,
    /// 2x2, assentos alternando equipe: 0 e 2 contra 1 e 3.
    DoisVsDois,
}

impl Modo {
    pub const fn jogadores(self) -> usize {
        match self {
            Modo::UmVsUm => 2,
            Modo::DoisVsDois => 4,
        }
    }
}

/// A equipe de um assento. Assentos alternam, então em 2x2 o parceiro está a duas cadeiras.
pub const fn equipe_de(assento: Assento) -> Equipe {
    (assento % 2) as Equipe
}

/// A escada de pontos (R-06): `1 → 3 → 6 → 9 → 12`. `None` no topo.
pub const fn proximo_valor(valor: u8) -> Option<u8> {
    match valor {
        1 => Some(3),
        3 => Some(6),
        6 => Some(9),
        9 => Some(12),
        _ => None,
    }
}

/// Quanto ganha quem pediu, se o adversário correr (R-06): sempre o **valor anterior**.
pub const fn valor_se_correr(proposto: u8) -> u8 {
    match proposto {
        6 => 3,
        9 => 6,
        12 => 9,
        _ => 1, // proposto == 3
    }
}

/// O nome que a mesa grita em cada nível da escada.
pub const fn nome_do_pedido(proposto: u8) -> &'static str {
    match proposto {
        3 => "truco",
        6 => "seis",
        9 => "nove",
        12 => "doze",
        _ => "?",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Especial {
    Normal,
    /// R-10: uma equipe em 11. Ela vê a mão do parceiro e decide jogar ou correr.
    MaoDeOnze(Equipe),
    /// R-11: as duas em 11. Às cegas, vale 3, ninguém corre.
    MaoDeFerro,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Aposta {
    /// Valor proposto (3, 6, 9 ou 12).
    pub proposto: u8,
    pub pedinte: Assento,
    /// Quem deve responder — o adversário imediatamente à esquerda de quem pediu (D-04).
    pub responde: Assento,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "fase", rename_all = "snake_case")]
pub enum Fase {
    /// R-10: a equipe em 11 ainda não disse se joga.
    DecidirOnze {
        equipe: Equipe,
        decide: Assento,
    },
    Jogando {
        vez: Assento,
    },
    Respondendo(Aposta),
    Encerrada {
        vencedora: Option<Equipe>,
        pontos: u8,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Jogada {
    pub assento: Assento,
    pub carta: Carta,
    /// R-12: carta virada, não vale nada. O caractere **não** é revelado aos outros.
    pub encoberta: bool,
}

impl Jogada {
    fn forca(&self, vira: Carta) -> Forca {
        if self.encoberta {
            FORCA_ENCOBERTA
        } else {
            self.carta.forca(vira)
        }
    }
}

/// Chega do cliente (`{"acao":"responder","resposta":"aceito"}`) e volta no log, logo precisa
/// dos dois lados.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resposta {
    Aceito,
    Correr,
    Aumentar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acao {
    Jogar {
        carta: Carta,
        encoberta: bool,
    },
    Pedir,
    Responder(Resposta),
    /// R-10: a equipe em 11 decide.
    DecidirOnze {
        aceita: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Erro {
    #[error("nao e a sua vez")]
    NaoEhSuaVez,
    #[error("acao invalida nesta fase da mao")]
    FaseErrada,
    #[error("voce nao tem essa carta")]
    CartaNaoEstaNaMao,
    #[error("nao se pode esconder carta na primeira rodada")]
    EncobertaNaPrimeiraRodada,
    #[error("nao se pode pedir aumento em mao de onze nem em mao de ferro")]
    SemAumentoNaMaoEspecial,
    #[error("a mao ja vale 12, nao ha o que aumentar")]
    EscadaNoTopo,
    #[error("sua equipe pediu o ultimo aumento; espere o adversario")]
    EquipeBloqueada,
    #[error("a partida ja terminou")]
    PartidaEncerrada,
}

/// Evento para o log da mesa. É o que vai ao cliente além do estado.
///
/// `CartaJogada.carta` é `None` quando a carta foi encoberta: o caractere não sai do servidor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "evento", rename_all = "snake_case")]
pub enum Evento {
    MaoIniciada {
        vira: Carta,
        valor: u8,
        especial: Especial,
        puxador: Assento,
    },
    CartaJogada {
        assento: Assento,
        carta: Option<Carta>,
        encoberta: bool,
    },
    RodadaResolvida {
        numero: u8,
        vencedora: Option<Equipe>,
    },
    Pedido {
        assento: Assento,
        proposto: u8,
        nome: &'static str,
    },
    Respondido {
        assento: Assento,
        resposta: Resposta,
    },
    OnzeDecidida {
        equipe: Equipe,
        aceita: bool,
    },
    MaoEncerrada {
        vencedora: Option<Equipe>,
        pontos: u8,
        motivo: &'static str,
    },
    PartidaEncerrada {
        vencedora: Equipe,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resolucao {
    Continua,
    Vencedor(Equipe),
    /// R-08: "Se todas as três rodadas empatarem, ninguém ganha ponto."
    Ninguem,
}

/// Os critérios de desempate de R-08, literais, numa única função total.
///
/// Consequência não óbvia que isto codifica: a mão pode terminar na **segunda** rodada mesmo
/// sem ninguém ter duas vitórias — basta empate na 1ª e vitória na 2ª, ou vitória na 1ª e
/// empate na 2ª. Nesses casos a terceira carta nunca é jogada.
fn resolver(rodadas: &[Option<Equipe>]) -> Resolucao {
    use Resolucao::*;
    match rodadas {
        [] | [_] => Continua,
        // "Se houver empate na primeira rodada, o vencedor da segunda ganha a mão"
        [None, Some(t)] => Vencedor(*t),
        // "Se houver empate na segunda rodada, o vencedor da primeira ganha a mão"
        [Some(a), None] => Vencedor(*a),
        [Some(a), Some(b)] if a == b => Vencedor(*a),
        // 1 a 1, ou empate duplo: vai para a terceira
        [Some(_), Some(_)] | [None, None] => Continua,
        // "Se houver empate na primeira e na segunda, o vencedor da terceira ganha a mão"
        [None, None, Some(t)] => Vencedor(*t),
        // "Se todas as três rodadas empatarem, ninguém ganha ponto"
        [None, None, None] => Ninguem,
        // "Se houver empate na terceira rodada, o vencedor da primeira ganha a mão"
        [Some(a), Some(_), None] => Vencedor(*a),
        [Some(_), Some(_), Some(c)] => Vencedor(*c),
        _ => Continua,
    }
}

#[derive(Debug, Clone)]
pub struct Mao {
    pub vira: Carta,
    /// Cartas na mão de cada assento. Índice = assento.
    pub cartas: Vec<Vec<Carta>>,
    /// Jogadas da rodada corrente, na ordem em que foram feitas.
    pub mesa: Vec<Jogada>,
    /// Resultado das rodadas já fechadas. `None` = empate.
    pub rodadas: Vec<Option<Equipe>>,
    /// Quem puxa a rodada corrente (D-01, D-05).
    pub puxador: Assento,
    pub valor: u8,
    pub especial: Especial,
    pub fase: Fase,
    /// Equipe que pediu o último aumento aceito e por isso não pode pedir de novo (R-07).
    pub bloqueada: Option<Equipe>,
}

#[derive(Debug, Clone)]
pub struct Partida {
    pub modo: Modo,
    pub pontos: [u8; 2],
    /// D-01: roda no sentido horário a cada mão.
    pub embaralhador: Assento,
    pub mao: Mao,
    pub vencedora: Option<Equipe>,
    pub numero_da_mao: u32,
    rng: ChaCha8Rng,
}

impl Partida {
    /// Nova partida. A semente existe para o teste ser reprodutível; em produção vem de
    /// `rand::rng()`, ver `Partida::nova`.
    pub fn com_semente(modo: Modo, semente: u64) -> Self {
        let mut rng = ChaCha8Rng::seed_from_u64(semente);
        // D-01: o embaralhador da primeira mão é o assento 0, e o "mão" é o seguinte.
        let embaralhador = 0;
        let mao = distribuir(modo, embaralhador, Especial::Normal, 1, &mut rng);
        Partida {
            modo,
            pontos: [0, 0],
            embaralhador,
            mao,
            vencedora: None,
            numero_da_mao: 1,
            rng,
        }
    }

    pub fn nova(modo: Modo) -> Self {
        Self::com_semente(modo, rand::random())
    }

    pub fn jogadores(&self) -> usize {
        self.modo.jogadores()
    }

    /// O evento de abertura da mão corrente, para quem entra ou reconecta.
    pub fn evento_de_abertura(&self) -> Evento {
        Evento::MaoIniciada {
            vira: self.mao.vira,
            valor: self.mao.valor,
            especial: self.mao.especial,
            puxador: self.mao.puxador,
        }
    }

    /// Começa a mão seguinte. Só vale com a mão corrente encerrada e a partida em aberto.
    ///
    /// Fica separado de `aplicar` de propósito: a mesa precisa de uma pausa entre a mão que
    /// acabou e a próxima, senão o jogador não vê o resultado.
    pub fn proxima_mao(&mut self) -> Option<Evento> {
        if self.vencedora.is_some() || !matches!(self.mao.fase, Fase::Encerrada { .. }) {
            return None;
        }
        self.embaralhador = (self.embaralhador + 1) % self.jogadores();
        self.numero_da_mao += 1;
        let especial = self.especial_desta_mao();
        let valor = match especial {
            Especial::Normal => 1,
            // R-10 e R-11: a mão especial já nasce valendo 3.
            _ => 3,
        };
        self.mao = distribuir(self.modo, self.embaralhador, especial, valor, &mut self.rng);
        Some(self.evento_de_abertura())
    }

    /// R-10 e R-11 decidem a natureza da mão pela pontuação de **antes** de distribuir.
    fn especial_desta_mao(&self) -> Especial {
        let [a, b] = self.pontos;
        match (a >= PONTOS_MAO_DE_ONZE, b >= PONTOS_MAO_DE_ONZE) {
            (true, true) => Especial::MaoDeFerro,
            (true, false) => Especial::MaoDeOnze(0),
            (false, true) => Especial::MaoDeOnze(1),
            (false, false) => Especial::Normal,
        }
    }

    /// Aplica uma ação de um assento. Erro aqui é recusa, nunca estado parcial:
    /// toda validação acontece antes de qualquer mutação.
    pub fn aplicar(&mut self, assento: Assento, acao: Acao) -> Result<Vec<Evento>, Erro> {
        if self.vencedora.is_some() {
            return Err(Erro::PartidaEncerrada);
        }
        match acao {
            Acao::DecidirOnze { aceita } => self.decidir_onze(assento, aceita),
            Acao::Jogar { carta, encoberta } => self.jogar(assento, carta, encoberta),
            Acao::Pedir => self.pedir(assento),
            Acao::Responder(r) => self.responder(assento, r),
        }
    }

    fn decidir_onze(&mut self, assento: Assento, aceita: bool) -> Result<Vec<Evento>, Erro> {
        let Fase::DecidirOnze { equipe, decide } = self.mao.fase else {
            return Err(Erro::FaseErrada);
        };
        if assento != decide {
            return Err(Erro::NaoEhSuaVez);
        }
        let mut evs = vec![Evento::OnzeDecidida { equipe, aceita }];
        if aceita {
            self.mao.fase = Fase::Jogando {
                vez: self.mao.puxador,
            };
        } else {
            // R-10: "se a dupla correr, a dupla adversária ganha um ponto"
            let adversaria = 1 - equipe;
            evs.extend(self.encerrar_mao(Some(adversaria), 1, "correu da mao de onze"));
        }
        Ok(evs)
    }

    fn jogar(
        &mut self,
        assento: Assento,
        carta: Carta,
        encoberta: bool,
    ) -> Result<Vec<Evento>, Erro> {
        let Fase::Jogando { vez } = self.mao.fase else {
            return Err(Erro::FaseErrada);
        };
        if vez != assento {
            return Err(Erro::NaoEhSuaVez);
        }
        // R-12: "não é permitido esconder a carta na primeira rodada de cada mão"
        if encoberta && self.mao.rodadas.is_empty() {
            return Err(Erro::EncobertaNaPrimeiraRodada);
        }
        let pos = self.mao.cartas[assento]
            .iter()
            .position(|c| *c == carta)
            .ok_or(Erro::CartaNaoEstaNaMao)?;

        // Daqui para baixo não há mais erro possível: valida tudo, muta depois.
        self.mao.cartas[assento].remove(pos);
        self.mao.mesa.push(Jogada {
            assento,
            carta,
            encoberta,
        });
        let mut evs = vec![Evento::CartaJogada {
            assento,
            carta: (!encoberta).then_some(carta),
            encoberta,
        }];

        if self.mao.mesa.len() < self.jogadores() {
            self.mao.fase = Fase::Jogando {
                vez: self.proximo_assento(assento),
            };
            return Ok(evs);
        }

        evs.extend(self.fechar_rodada());
        Ok(evs)
    }

    fn fechar_rodada(&mut self) -> Vec<Evento> {
        let vira = self.mao.vira;
        let melhor = self
            .mao
            .mesa
            .iter()
            .map(|j| j.forca(vira))
            .max()
            .unwrap_or(FORCA_ENCOBERTA);
        let empatadas: Vec<&Jogada> = self
            .mao
            .mesa
            .iter()
            .filter(|j| j.forca(vira) == melhor)
            .collect();

        // Duas cartas de mesma força do **mesmo** time não é empate: o time levou.
        // Só é empate quando a maior força aparece nos dois lados (R-02, R-08).
        let equipes: Vec<Equipe> = empatadas.iter().map(|j| equipe_de(j.assento)).collect();
        let vencedora = if equipes.iter().all(|e| *e == equipes[0]) {
            Some(equipes[0])
        } else {
            None
        };
        // D-01: quem levou puxa a próxima. D-05: no empate, quem puxou puxa de novo.
        let proximo_puxador = match vencedora {
            Some(_) => empatadas[0].assento,
            None => self.mao.puxador,
        };

        self.mao.rodadas.push(vencedora);
        self.mao.mesa.clear();
        let numero = self.mao.rodadas.len() as u8;
        let mut evs = vec![Evento::RodadaResolvida { numero, vencedora }];

        match resolver(&self.mao.rodadas) {
            Resolucao::Continua => {
                self.mao.puxador = proximo_puxador;
                self.mao.fase = Fase::Jogando {
                    vez: proximo_puxador,
                };
            }
            Resolucao::Vencedor(e) => {
                let pontos = self.mao.valor;
                evs.extend(self.encerrar_mao(Some(e), pontos, "levou a mao"));
            }
            Resolucao::Ninguem => {
                evs.extend(self.encerrar_mao(None, 0, "as tres rodadas empataram"));
            }
        }
        evs
    }

    fn pedir(&mut self, assento: Assento) -> Result<Vec<Evento>, Erro> {
        let Fase::Jogando { vez } = self.mao.fase else {
            return Err(Erro::FaseErrada);
        };
        // R-07: "só pode ser feito na vez do jogador", e antes de jogar a carta.
        if vez != assento {
            return Err(Erro::NaoEhSuaVez);
        }
        // R-10/R-11 via F-02: em mão de onze e mão de ferro não há aumento.
        if self.mao.especial != Especial::Normal {
            return Err(Erro::SemAumentoNaMaoEspecial);
        }
        if self.mao.bloqueada == Some(equipe_de(assento)) {
            return Err(Erro::EquipeBloqueada);
        }
        let proposto = proximo_valor(self.mao.valor).ok_or(Erro::EscadaNoTopo)?;
        let responde = self.proximo_assento(assento);
        self.mao.fase = Fase::Respondendo(Aposta {
            proposto,
            pedinte: assento,
            responde,
        });
        Ok(vec![Evento::Pedido {
            assento,
            proposto,
            nome: nome_do_pedido(proposto),
        }])
    }

    fn responder(&mut self, assento: Assento, r: Resposta) -> Result<Vec<Evento>, Erro> {
        let Fase::Respondendo(aposta) = self.mao.fase else {
            return Err(Erro::FaseErrada);
        };
        if aposta.responde != assento {
            return Err(Erro::NaoEhSuaVez);
        }
        // Validar o aumento antes de emitir qualquer evento.
        let subida = if r == Resposta::Aumentar {
            Some(proximo_valor(aposta.proposto).ok_or(Erro::EscadaNoTopo)?)
        } else {
            None
        };

        let mut evs = vec![Evento::Respondido {
            assento,
            resposta: r,
        }];
        match r {
            Resposta::Aceito => {
                self.mao.valor = aposta.proposto;
                // R-07: quem pediu não pode subir de novo sozinho; espera o adversário.
                self.mao.bloqueada = Some(equipe_de(aposta.pedinte));
                // Quem pediu ainda tem de jogar a carta: o pedido veio antes da jogada.
                self.mao.fase = Fase::Jogando {
                    vez: aposta.pedinte,
                };
            }
            Resposta::Correr => {
                // R-06: correr concede a quem pediu o valor **anterior** da escada.
                let equipe = equipe_de(aposta.pedinte);
                let pontos = valor_se_correr(aposta.proposto);
                evs.extend(self.encerrar_mao(Some(equipe), pontos, "adversario correu"));
            }
            Resposta::Aumentar => {
                let proposto = subida.expect("validado acima");
                self.mao.fase = Fase::Respondendo(Aposta {
                    proposto,
                    pedinte: assento,
                    responde: aposta.pedinte,
                });
                evs.push(Evento::Pedido {
                    assento,
                    proposto,
                    nome: nome_do_pedido(proposto),
                });
            }
        }
        Ok(evs)
    }

    fn encerrar_mao(
        &mut self,
        vencedora: Option<Equipe>,
        pontos: u8,
        motivo: &'static str,
    ) -> Vec<Evento> {
        if let Some(e) = vencedora {
            self.pontos[e as usize] = self.pontos[e as usize].saturating_add(pontos);
        }
        self.mao.fase = Fase::Encerrada { vencedora, pontos };
        let mut evs = vec![Evento::MaoEncerrada {
            vencedora,
            pontos,
            motivo,
        }];
        // R-09: 12 pontos vencem a partida.
        if let Some(e) = vencedora {
            if self.pontos[e as usize] >= PONTOS_PARA_VENCER {
                self.vencedora = Some(e);
                evs.push(Evento::PartidaEncerrada { vencedora: e });
            }
        }
        evs
    }

    fn proximo_assento(&self, a: Assento) -> Assento {
        (a + 1) % self.jogadores()
    }

    /// R-10: durante a decisão da mão de onze, e **somente** então, a equipe em 11 vê a mão do
    /// parceiro. Fora disso ninguém vê carta alheia — é esta função que garante o isolamento.
    pub fn pode_ver(&self, observador: Assento, alvo: Assento) -> bool {
        if observador == alvo {
            return true;
        }
        match self.mao.fase {
            Fase::DecidirOnze { equipe, .. } => {
                equipe_de(observador) == equipe && equipe_de(alvo) == equipe
            }
            _ => false,
        }
    }
}

/// Distribui: embaralha as 40, 3 cartas por jogador, e a seguinte é a vira (R-03, R-05).
fn distribuir(
    modo: Modo,
    embaralhador: Assento,
    especial: Especial,
    valor: u8,
    rng: &mut ChaCha8Rng,
) -> Mao {
    let n = modo.jogadores();
    let mut b = baralho();
    b.shuffle(rng);
    let cartas: Vec<Vec<Carta>> = (0..n).map(|i| b[i * 3..i * 3 + 3].to_vec()).collect();
    let vira = b[n * 3];
    // D-01: a primeira rodada é puxada por quem está à esquerda do embaralhador.
    let puxador = (embaralhador + 1) % n;
    let fase = match especial {
        // R-10: a equipe em 11 decide antes de qualquer carta ser jogada. Quem decide é o
        // membro dessa equipe que puxa a mão — escolha determinística, ver D-04.
        Especial::MaoDeOnze(equipe) => Fase::DecidirOnze {
            equipe,
            decide: (0..n)
                .map(|i| (puxador + i) % n)
                .find(|a| equipe_de(*a) == equipe)
                .unwrap_or(puxador),
        },
        _ => Fase::Jogando { vez: puxador },
    };
    Mao {
        vira,
        cartas,
        mesa: Vec::new(),
        rodadas: Vec::new(),
        puxador,
        valor,
        especial,
        fase,
        bloqueada: None,
    }
}

#[cfg(test)]
mod testes {
    use super::*;
    use crate::cartas::{Naipe, Valor};
    use std::collections::HashSet;

    const fn c(v: Valor, n: Naipe) -> Carta {
        Carta::nova(v, n)
    }

    /// Força um cenário. Os testes de regra não podem depender do embaralhamento.
    fn forjar(modo: Modo, vira: Carta, maos: Vec<Vec<Carta>>, puxador: Assento) -> Partida {
        let mut p = Partida::com_semente(modo, 7);
        p.mao.vira = vira;
        p.mao.cartas = maos;
        p.mao.puxador = puxador;
        p.mao.fase = Fase::Jogando { vez: puxador };
        p
    }

    fn jogar(p: &mut Partida, assento: Assento, carta: Carta) -> Vec<Evento> {
        p.aplicar(
            assento,
            Acao::Jogar {
                carta,
                encoberta: false,
            },
        )
        .unwrap_or_else(|e| panic!("assento {assento} nao conseguiu jogar {carta}: {e}"))
    }

    // ----- R-06: a escada de pontos -----

    /// A tabela de R-06, inclusive o ponto que refutei em F-01: truco aceito vale **3**.
    #[test]
    fn escada_de_pontos_e_1_3_6_9_12() {
        assert_eq!(
            proximo_valor(1),
            Some(3),
            "truco aceito vale 3, nao 2 (ver refutado/R-01)"
        );
        assert_eq!(proximo_valor(3), Some(6));
        assert_eq!(proximo_valor(6), Some(9));
        assert_eq!(proximo_valor(9), Some(12));
        assert_eq!(proximo_valor(12), None, "nao ha nivel acima de doze");
    }

    /// R-06: correr concede sempre o valor anterior da escada.
    #[test]
    fn correr_concede_o_valor_anterior() {
        assert_eq!(valor_se_correr(3), 1);
        assert_eq!(valor_se_correr(6), 3);
        assert_eq!(valor_se_correr(9), 6);
        assert_eq!(valor_se_correr(12), 9);
    }

    // ----- R-08: os cinco critérios de desempate, um a um -----

    #[test]
    fn tabela_de_desempate_r08() {
        use Resolucao::*;
        assert_eq!(resolver(&[]), Continua);
        assert_eq!(resolver(&[Some(0)]), Continua);
        assert_eq!(resolver(&[None]), Continua);
        // "Se houver empate na primeira rodada, o vencedor da segunda ganha a mão"
        assert_eq!(resolver(&[None, Some(1)]), Vencedor(1));
        // "Se houver empate na segunda rodada, o vencedor da primeira ganha a mão"
        assert_eq!(resolver(&[Some(0), None]), Vencedor(0));
        // duas rodadas para o mesmo time
        assert_eq!(resolver(&[Some(1), Some(1)]), Vencedor(1));
        // 1 a 1 vai para a terceira
        assert_eq!(resolver(&[Some(0), Some(1)]), Continua);
        assert_eq!(resolver(&[None, None]), Continua);
        // "Se houver empate na primeira e na segunda, o vencedor da terceira ganha a mão"
        assert_eq!(resolver(&[None, None, Some(0)]), Vencedor(0));
        // "Se houver empate na terceira rodada, o vencedor da primeira ganha a mão"
        assert_eq!(resolver(&[Some(1), Some(0), None]), Vencedor(1));
        assert_eq!(resolver(&[Some(0), Some(1), Some(1)]), Vencedor(1));
        // "Se todas as três rodadas empatarem, ninguém ganha ponto"
        assert_eq!(resolver(&[None, None, None]), Ninguem);
    }

    /// A consequência não óbvia de R-08: empate na 2ª encerra a mão **sem** jogar a 3ª.
    #[test]
    fn empate_na_segunda_encerra_a_mao_sem_terceira_rodada() {
        let vira = c(Valor::Quatro, Naipe::Ouros); // manilha = 5
        let mut p = forjar(
            Modo::UmVsUm,
            vira,
            vec![
                vec![
                    c(Valor::Tres, Naipe::Espadas),
                    c(Valor::Sete, Naipe::Espadas),
                    c(Valor::Seis, Naipe::Espadas),
                ],
                vec![
                    c(Valor::Dois, Naipe::Copas),
                    c(Valor::Sete, Naipe::Copas),
                    c(Valor::Quatro, Naipe::Copas),
                ],
            ],
            0,
        );
        jogar(&mut p, 0, c(Valor::Tres, Naipe::Espadas)); // 3 bate 2
        jogar(&mut p, 1, c(Valor::Dois, Naipe::Copas));
        assert_eq!(p.mao.rodadas, vec![Some(0)]);
        jogar(&mut p, 0, c(Valor::Sete, Naipe::Espadas)); // 7 x 7 = empate
        jogar(&mut p, 1, c(Valor::Sete, Naipe::Copas));
        assert_eq!(p.mao.rodadas, vec![Some(0), None]);
        assert!(
            matches!(
                p.mao.fase,
                Fase::Encerrada {
                    vencedora: Some(0),
                    pontos: 1
                }
            ),
            "R-08: vitoria na 1a + empate na 2a encerra a mao, fase ficou {:?}",
            p.mao.fase
        );
        assert_eq!(p.pontos, [1, 0]);
        // e a terceira carta continua na mão de cada um
        assert_eq!(p.mao.cartas[0].len(), 1);
    }

    /// R-02 produz empate, mas duas cartas iguais da **mesma** equipe não empatam a rodada.
    #[test]
    fn forca_igual_na_mesma_equipe_nao_e_empate() {
        let vira = c(Valor::Quatro, Naipe::Ouros);
        let mut p = forjar(
            Modo::DoisVsDois,
            vira,
            vec![
                vec![c(Valor::Tres, Naipe::Espadas); 3],
                vec![c(Valor::Dois, Naipe::Copas); 3],
                vec![c(Valor::Tres, Naipe::Copas); 3], // parceiro do assento 0, mesma força
                vec![c(Valor::Dois, Naipe::Ouros); 3],
            ],
            0,
        );
        jogar(&mut p, 0, c(Valor::Tres, Naipe::Espadas));
        jogar(&mut p, 1, c(Valor::Dois, Naipe::Copas));
        jogar(&mut p, 2, c(Valor::Tres, Naipe::Copas));
        jogar(&mut p, 3, c(Valor::Dois, Naipe::Ouros));
        assert_eq!(
            p.mao.rodadas,
            vec![Some(0)],
            "dois 3 da mesma equipe: a equipe levou, nao empatou"
        );
    }

    // ----- R-07: quem pede, quando, e o bloqueio -----

    #[test]
    fn truco_aceito_poe_a_mao_em_tres_e_devolve_a_vez_a_quem_pediu() {
        let vira = c(Valor::Quatro, Naipe::Ouros);
        let mut p = forjar(
            Modo::UmVsUm,
            vira,
            vec![
                vec![c(Valor::Tres, Naipe::Espadas); 3],
                vec![c(Valor::Dois, Naipe::Copas); 3],
            ],
            0,
        );
        p.aplicar(0, Acao::Pedir).unwrap();
        assert!(matches!(p.mao.fase, Fase::Respondendo(a) if a.proposto == 3 && a.responde == 1));
        p.aplicar(1, Acao::Responder(Resposta::Aceito)).unwrap();
        assert_eq!(p.mao.valor, 3);
        assert!(
            matches!(p.mao.fase, Fase::Jogando { vez: 0 }),
            "R-07: o pedido vem antes da jogada, quem pediu ainda tem de jogar"
        );
        // R-07: a equipe que pediu fica bloqueada
        assert_eq!(p.aplicar(0, Acao::Pedir), Err(Erro::EquipeBloqueada));
    }

    #[test]
    fn correr_do_truco_da_um_ponto_a_quem_pediu() {
        let vira = c(Valor::Quatro, Naipe::Ouros);
        let mut p = forjar(
            Modo::UmVsUm,
            vira,
            vec![
                vec![c(Valor::Tres, Naipe::Espadas); 3],
                vec![c(Valor::Dois, Naipe::Copas); 3],
            ],
            0,
        );
        p.aplicar(0, Acao::Pedir).unwrap();
        p.aplicar(1, Acao::Responder(Resposta::Correr)).unwrap();
        assert_eq!(
            p.pontos,
            [1, 0],
            "R-06: truco corrido = 1 ponto a quem pediu"
        );
        assert!(matches!(p.mao.fase, Fase::Encerrada { pontos: 1, .. }));
    }

    /// A escada inteira, subida por re-aumento, e o teto em 12.
    #[test]
    fn escada_sobe_por_reaumento_e_trava_no_doze() {
        let vira = c(Valor::Quatro, Naipe::Ouros);
        let mut p = forjar(
            Modo::UmVsUm,
            vira,
            vec![
                vec![c(Valor::Tres, Naipe::Espadas); 3],
                vec![c(Valor::Dois, Naipe::Copas); 3],
            ],
            0,
        );
        p.aplicar(0, Acao::Pedir).unwrap(); // truco: 3
        p.aplicar(1, Acao::Responder(Resposta::Aumentar)).unwrap(); // seis: 6
        assert!(matches!(p.mao.fase, Fase::Respondendo(a) if a.proposto == 6 && a.responde == 0));
        p.aplicar(0, Acao::Responder(Resposta::Aumentar)).unwrap(); // nove: 9
        p.aplicar(1, Acao::Responder(Resposta::Aumentar)).unwrap(); // doze: 12
        assert!(matches!(p.mao.fase, Fase::Respondendo(a) if a.proposto == 12 && a.responde == 0));
        assert_eq!(
            p.aplicar(0, Acao::Responder(Resposta::Aumentar)),
            Err(Erro::EscadaNoTopo),
            "R-06: nao existe nivel acima de doze"
        );
        // correr do doze concede nove a quem pediu o doze (assento 1, equipe 1)
        p.aplicar(0, Acao::Responder(Resposta::Correr)).unwrap();
        assert_eq!(p.pontos, [0, 9]);
    }

    #[test]
    fn so_se_pede_na_propria_vez() {
        let vira = c(Valor::Quatro, Naipe::Ouros);
        let mut p = forjar(
            Modo::UmVsUm,
            vira,
            vec![
                vec![c(Valor::Tres, Naipe::Espadas); 3],
                vec![c(Valor::Dois, Naipe::Copas); 3],
            ],
            0,
        );
        assert_eq!(p.aplicar(1, Acao::Pedir), Err(Erro::NaoEhSuaVez), "R-07");
        assert_eq!(
            p.aplicar(
                1,
                Acao::Jogar {
                    carta: c(Valor::Dois, Naipe::Copas),
                    encoberta: false
                }
            ),
            Err(Erro::NaoEhSuaVez)
        );
    }

    #[test]
    fn nao_se_joga_carta_que_nao_se_tem() {
        let vira = c(Valor::Quatro, Naipe::Ouros);
        let mut p = forjar(
            Modo::UmVsUm,
            vira,
            vec![
                vec![c(Valor::Tres, Naipe::Espadas); 3],
                vec![c(Valor::Dois, Naipe::Copas); 3],
            ],
            0,
        );
        assert_eq!(
            p.aplicar(
                0,
                Acao::Jogar {
                    carta: c(Valor::As, Naipe::Paus),
                    encoberta: false
                }
            ),
            Err(Erro::CartaNaoEstaNaMao),
            "o cliente nao escolhe carta que nao esta na mao dele"
        );
    }

    // ----- R-12: carta encoberta -----

    #[test]
    fn encoberta_proibida_na_primeira_rodada_e_nao_revela_o_caractere() {
        let vira = c(Valor::Quatro, Naipe::Ouros);
        let mut p = forjar(
            Modo::UmVsUm,
            vira,
            vec![
                vec![
                    c(Valor::Tres, Naipe::Espadas),
                    c(Valor::Seis, Naipe::Espadas),
                    c(Valor::Sete, Naipe::Espadas),
                ],
                vec![
                    c(Valor::Dois, Naipe::Copas),
                    c(Valor::Quatro, Naipe::Copas),
                    c(Valor::Cinco, Naipe::Paus),
                ],
            ],
            0,
        );
        assert_eq!(
            p.aplicar(
                0,
                Acao::Jogar {
                    carta: c(Valor::Tres, Naipe::Espadas),
                    encoberta: true
                }
            ),
            Err(Erro::EncobertaNaPrimeiraRodada),
            "R-12"
        );
        jogar(&mut p, 0, c(Valor::Tres, Naipe::Espadas));
        jogar(&mut p, 1, c(Valor::Dois, Naipe::Copas));
        // segunda rodada: agora pode
        let evs = p
            .aplicar(
                0,
                Acao::Jogar {
                    carta: c(Valor::Seis, Naipe::Espadas),
                    encoberta: true,
                },
            )
            .expect("R-12 permite encobrir da segunda rodada em diante");
        assert_eq!(
            evs[0],
            Evento::CartaJogada {
                assento: 0,
                carta: None,
                encoberta: true
            },
            "o caractere de uma carta encoberta nao pode sair do servidor"
        );
        // e a encoberta perde: o 4 do adversário leva a rodada
        jogar(&mut p, 1, c(Valor::Quatro, Naipe::Copas));
        assert_eq!(p.mao.rodadas, vec![Some(0), Some(1)]);
    }

    // ----- R-10 e R-11: mão de onze e mão de ferro -----

    fn com_pontos(modo: Modo, pontos: [u8; 2]) -> Partida {
        let mut p = Partida::com_semente(modo, 11);
        p.pontos = pontos;
        p.mao.fase = Fase::Encerrada {
            vencedora: None,
            pontos: 0,
        };
        p.proxima_mao();
        p
    }

    #[test]
    fn mao_de_onze_nasce_valendo_tres_e_pede_decisao() {
        let p = com_pontos(Modo::DoisVsDois, [11, 4]);
        assert_eq!(p.mao.especial, Especial::MaoDeOnze(0), "R-10");
        assert_eq!(p.mao.valor, 3, "R-10: a mao de onze vale tres");
        let Fase::DecidirOnze { equipe, decide } = p.mao.fase else {
            panic!("R-10 exige decisao antes de jogar, fase = {:?}", p.mao.fase)
        };
        assert_eq!(equipe, 0);
        assert_eq!(
            equipe_de(decide),
            0,
            "quem decide tem de ser da equipe em 11"
        );
    }

    #[test]
    fn correr_da_mao_de_onze_da_um_ponto_ao_adversario() {
        let mut p = com_pontos(Modo::DoisVsDois, [11, 4]);
        let Fase::DecidirOnze { decide, .. } = p.mao.fase else {
            unreachable!()
        };
        p.aplicar(decide, Acao::DecidirOnze { aceita: false })
            .unwrap();
        assert_eq!(p.pontos, [11, 5], "R-10: correu, adversario ganha 1");
    }

    #[test]
    fn aceitar_a_mao_de_onze_libera_o_jogo_valendo_tres() {
        let mut p = com_pontos(Modo::DoisVsDois, [11, 4]);
        let Fase::DecidirOnze { decide, .. } = p.mao.fase else {
            unreachable!()
        };
        p.aplicar(decide, Acao::DecidirOnze { aceita: true })
            .unwrap();
        assert!(matches!(p.mao.fase, Fase::Jogando { .. }));
        assert_eq!(p.mao.valor, 3);
        // R-10 via F-02: em mão de onze não se pede aumento
        let Fase::Jogando { vez } = p.mao.fase else {
            unreachable!()
        };
        assert_eq!(
            p.aplicar(vez, Acao::Pedir),
            Err(Erro::SemAumentoNaMaoEspecial)
        );
    }

    #[test]
    fn mao_de_ferro_em_onze_a_onze_joga_as_cegas_valendo_tres() {
        let p = com_pontos(Modo::DoisVsDois, [11, 11]);
        assert_eq!(p.mao.especial, Especial::MaoDeFerro, "R-11");
        assert_eq!(p.mao.valor, 3);
        assert!(
            matches!(p.mao.fase, Fase::Jogando { .. }),
            "R-11: na mao de ferro ninguem decide nem corre"
        );
    }

    /// R-10: a visibilidade da mão do parceiro é a **única** exceção ao isolamento, e vale só
    /// durante a decisão.
    #[test]
    fn isolamento_das_maos_e_a_unica_excecao_da_mao_de_onze() {
        let p = Partida::com_semente(Modo::DoisVsDois, 3);
        for obs in 0..4 {
            for alvo in 0..4 {
                assert_eq!(
                    p.pode_ver(obs, alvo),
                    obs == alvo,
                    "mao normal: so a propria"
                );
            }
        }
        let mut p = com_pontos(Modo::DoisVsDois, [11, 4]);
        // equipe 0 = assentos 0 e 2
        assert!(
            p.pode_ver(0, 2),
            "R-10: a equipe em 11 ve a mao do parceiro"
        );
        assert!(p.pode_ver(2, 0));
        assert!(!p.pode_ver(0, 1), "nunca se ve a mao do adversario");
        assert!(
            !p.pode_ver(1, 3),
            "a equipe que nao esta em 11 nao ganha visibilidade"
        );
        // depois de decidir, a visibilidade fecha
        let Fase::DecidirOnze { decide, .. } = p.mao.fase else {
            unreachable!()
        };
        p.aplicar(decide, Acao::DecidirOnze { aceita: true })
            .unwrap();
        assert!(
            !p.pode_ver(0, 2),
            "a visibilidade da mao de onze acaba com a decisao"
        );
    }

    // ----- distribuição e fim de partida -----

    #[test]
    fn distribuicao_da_tres_cartas_a_cada_um_mais_a_vira_sem_repetir() {
        for modo in [Modo::UmVsUm, Modo::DoisVsDois] {
            for semente in 0..50 {
                let p = Partida::com_semente(modo, semente);
                let n = modo.jogadores();
                assert_eq!(p.mao.cartas.len(), n);
                let mut vistas: HashSet<Carta> = HashSet::new();
                for m in &p.mao.cartas {
                    assert_eq!(m.len(), 3, "R-05: tres cartas por jogador");
                    for c in m {
                        assert!(vistas.insert(*c), "carta repetida na mesa: {c}");
                    }
                }
                assert!(
                    vistas.insert(p.mao.vira),
                    "a vira saiu repetida: {}",
                    p.mao.vira
                );
            }
        }
    }

    /// Partida inteira com um bot bobo (joga a primeira carta legal, nunca truca).
    /// Prova que o motor termina, que ninguém passa de 12 sem fim, e que não há laço infinito.
    #[test]
    fn partida_inteira_termina_em_doze_pontos() {
        for modo in [Modo::UmVsUm, Modo::DoisVsDois] {
            for semente in 0..40 {
                let mut p = Partida::com_semente(modo, semente);
                let mut passos = 0;
                while p.vencedora.is_none() {
                    passos += 1;
                    assert!(
                        passos < 5000,
                        "motor nao termina (modo {modo:?}, semente {semente})"
                    );
                    match p.mao.fase {
                        Fase::DecidirOnze { decide, .. } => {
                            p.aplicar(decide, Acao::DecidirOnze { aceita: true })
                                .unwrap();
                        }
                        Fase::Jogando { vez } => {
                            let carta = p.mao.cartas[vez][0];
                            p.aplicar(
                                vez,
                                Acao::Jogar {
                                    carta,
                                    encoberta: false,
                                },
                            )
                            .unwrap();
                        }
                        Fase::Respondendo(a) => {
                            p.aplicar(a.responde, Acao::Responder(Resposta::Aceito))
                                .unwrap();
                        }
                        Fase::Encerrada { .. } => {
                            p.proxima_mao();
                        }
                    }
                }
                let e = p.vencedora.unwrap() as usize;
                assert!(
                    p.pontos[e] >= PONTOS_PARA_VENCER,
                    "R-09: venceu com {} pontos",
                    p.pontos[e]
                );
                assert!(
                    p.pontos[1 - e] < PONTOS_PARA_VENCER,
                    "os dois nao podem vencer"
                );
            }
        }
    }

    /// O mesmo bot, mas trucando sempre que pode: exercita a escada dentro de partidas reais.
    #[test]
    fn partida_inteira_com_truco_em_toda_oportunidade_tambem_termina() {
        for semente in 0..40 {
            let mut p = Partida::com_semente(Modo::DoisVsDois, semente);
            let mut passos = 0;
            while p.vencedora.is_none() {
                passos += 1;
                assert!(passos < 20000, "motor nao termina com truco agressivo");
                match p.mao.fase {
                    Fase::DecidirOnze { decide, .. } => {
                        p.aplicar(decide, Acao::DecidirOnze { aceita: true })
                            .unwrap();
                    }
                    Fase::Jogando { vez } => {
                        if p.aplicar(vez, Acao::Pedir).is_err() {
                            let carta = p.mao.cartas[vez][0];
                            p.aplicar(
                                vez,
                                Acao::Jogar {
                                    carta,
                                    encoberta: false,
                                },
                            )
                            .unwrap();
                        }
                    }
                    Fase::Respondendo(a) => {
                        let r = if a.proposto >= 9 {
                            Resposta::Aceito
                        } else {
                            Resposta::Aumentar
                        };
                        p.aplicar(a.responde, Acao::Responder(r)).unwrap();
                    }
                    Fase::Encerrada { .. } => {
                        p.proxima_mao();
                    }
                }
            }
            assert!(p.pontos.iter().any(|x| *x >= PONTOS_PARA_VENCER));
        }
    }

    #[test]
    fn acao_depois_do_fim_da_partida_e_recusada() {
        let mut p = Partida::com_semente(Modo::UmVsUm, 1);
        p.vencedora = Some(0);
        assert_eq!(p.aplicar(0, Acao::Pedir), Err(Erro::PartidaEncerrada));
    }
}
