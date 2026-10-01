//! Cartas do truco paulista e a força de cada uma.
//!
//! O baralho tem 40 cartas (R-01) e a carta trafega no protocolo como o **próprio caractere
//! Unicode** do bloco *Playing Cards* (U+1F0A0..U+1F0DF) — nunca como string inventada.
//!
//! Regras citadas: ver `regras/truco-paulista.md` no repositório de pesquisa
//! <https://github.com/yryapu/poliorketikos-truco-paulista>.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// Naipe. A ordem da enum **é** a ordem de força entre manilhas (R-04):
/// `Ouros < Espadas < Copas < Paus`. Fora das manilhas o naipe não desempata (R-02).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Naipe {
    Ouros = 0,
    Espadas = 1,
    Copas = 2,
    Paus = 3,
}

/// Valor. A ordem da enum é a ordem de força das cartas comuns (R-02):
/// `4 < 5 < 6 < 7 < Q < J < K < A < 2 < 3`.
///
/// Note que `Dama` vem **antes** de `Valete`: no truco paulista a Q é mais fraca que o J.
/// É contraintuitivo e é a fonte que manda — F-01: "a 'Q' é mais fraca que o 'J'".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Valor {
    Quatro = 0,
    Cinco = 1,
    Seis = 2,
    Sete = 3,
    Dama = 4,
    Valete = 5,
    Rei = 6,
    As = 7,
    Dois = 8,
    Tres = 9,
}

impl Naipe {
    pub const TODOS: [Naipe; 4] = [Naipe::Ouros, Naipe::Espadas, Naipe::Copas, Naipe::Paus];

    /// Primeiro ponto de código do naipe no bloco *Playing Cards*.
    const fn base_unicode(self) -> u32 {
        match self {
            Naipe::Espadas => 0x1F0A0,
            Naipe::Copas => 0x1F0B0,
            Naipe::Ouros => 0x1F0C0,
            Naipe::Paus => 0x1F0D0,
        }
    }

    pub const fn nome(self) -> &'static str {
        match self {
            Naipe::Ouros => "ouros",
            Naipe::Espadas => "espadas",
            Naipe::Copas => "copas",
            Naipe::Paus => "paus",
        }
    }
}

impl Valor {
    pub const TODOS: [Valor; 10] = [
        Valor::Quatro,
        Valor::Cinco,
        Valor::Seis,
        Valor::Sete,
        Valor::Dama,
        Valor::Valete,
        Valor::Rei,
        Valor::As,
        Valor::Dois,
        Valor::Tres,
    ];

    /// Deslocamento dentro do naipe. **Não é o índice da enum** — o bloco Unicode numera
    /// A,2..10,J,C,Q,K e o truco pula 8, 9, 10 e o Cavaleiro (`C`, +0xC).
    ///
    /// Esta é a armadilha do bloco: um laço `base + i` contrabandearia
    /// `🂬 PLAYING CARD KNIGHT OF SPADES` para dentro do baralho.
    const fn deslocamento_unicode(self) -> u32 {
        match self {
            Valor::As => 0x1,
            Valor::Dois => 0x2,
            Valor::Tres => 0x3,
            Valor::Quatro => 0x4,
            Valor::Cinco => 0x5,
            Valor::Seis => 0x6,
            Valor::Sete => 0x7,
            Valor::Valete => 0xB,
            Valor::Dama => 0xD,
            Valor::Rei => 0xE,
        }
    }

    /// O valor seguinte na ordem de força, **circular** (R-03).
    ///
    /// A circularidade não é detalhe: F-01 avisa "quando a vira for o '3', as manilhas são as
    /// cartas '4'". Sem isto, vira `3` não produz manilha nenhuma.
    pub const fn proximo(self) -> Valor {
        match self {
            Valor::Quatro => Valor::Cinco,
            Valor::Cinco => Valor::Seis,
            Valor::Seis => Valor::Sete,
            Valor::Sete => Valor::Dama,
            Valor::Dama => Valor::Valete,
            Valor::Valete => Valor::Rei,
            Valor::Rei => Valor::As,
            Valor::As => Valor::Dois,
            Valor::Dois => Valor::Tres,
            Valor::Tres => Valor::Quatro,
        }
    }

    pub const fn rotulo(self) -> &'static str {
        match self {
            Valor::Quatro => "4",
            Valor::Cinco => "5",
            Valor::Seis => "6",
            Valor::Sete => "7",
            Valor::Dama => "Q",
            Valor::Valete => "J",
            Valor::Rei => "K",
            Valor::As => "A",
            Valor::Dois => "2",
            Valor::Tres => "3",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Carta {
    pub valor: Valor,
    pub naipe: Naipe,
}

/// Força de uma carta na mesa, dada a vira. Comparável por `>`; igualdade é **empate** (R-08).
///
/// Três faixas, escolhidas para não colidirem:
/// - `0` — carta encoberta (R-12): "passará a não valer nada".
/// - `10..=19` — carta comum, pela ordem de R-02. O naipe **não** entra.
/// - `100..=103` — manilha, pelo naipe de R-04. Duas manilhas nunca empatam.
pub type Forca = u8;

pub const FORCA_ENCOBERTA: Forca = 0;

impl Carta {
    pub const fn nova(valor: Valor, naipe: Naipe) -> Self {
        Carta { valor, naipe }
    }

    /// O caractere Unicode da carta. É isto que trafega no WebSocket.
    pub fn unicode(self) -> char {
        let cp = self.naipe.base_unicode() + self.valor.deslocamento_unicode();
        // Infalível por construção: todo ponto de código gerado está em 1F0A1..1F0DE,
        // que é faixa válida e não-surrogate. O expect documenta a invariante.
        char::from_u32(cp).expect("ponto de codigo do bloco Playing Cards e sempre valido")
    }

    /// Inverso de [`Carta::unicode`]. Recusa 8, 9, 10 e o Cavaleiro, que não existem no truco.
    pub fn do_unicode(c: char) -> Option<Carta> {
        let cp = c as u32;
        let naipe = match cp & 0xFFFF0 {
            0x1F0A0 => Naipe::Espadas,
            0x1F0B0 => Naipe::Copas,
            0x1F0C0 => Naipe::Ouros,
            0x1F0D0 => Naipe::Paus,
            _ => return None,
        };
        let desl = cp & 0xF;
        let valor = Valor::TODOS
            .into_iter()
            .find(|v| v.deslocamento_unicode() == desl)?;
        Some(Carta::nova(valor, naipe))
    }

    /// `true` se esta carta é manilha nesta mão (R-03).
    pub fn e_manilha(self, vira: Carta) -> bool {
        self.valor == vira.valor.proximo()
    }

    /// Força na mesa, dada a vira (R-02, R-03, R-04).
    pub fn forca(self, vira: Carta) -> Forca {
        if self.e_manilha(vira) {
            100 + self.naipe as Forca
        } else {
            10 + self.valor as Forca
        }
    }
}

impl fmt::Display for Carta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.unicode())
    }
}

/// No JSON do protocolo a carta é o caractere, e só. `{"carta":"🂡"}`.
impl Serialize for Carta {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(&self.unicode())
    }
}

impl<'de> Deserialize<'de> for Carta {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Carta, D::Error> {
        let s = String::deserialize(d)?;
        let mut cs = s.chars();
        let (Some(c), None) = (cs.next(), cs.next()) else {
            return Err(serde::de::Error::custom(format!(
                "carta deve ser exatamente um caractere do bloco Playing Cards, veio {s:?}"
            )));
        };
        Carta::do_unicode(c)
            .ok_or_else(|| serde::de::Error::custom(format!("{c:?} nao e carta do truco")))
    }
}

/// O baralho de 40 cartas (R-01): sem 8, 9, 10 e sem curinga.
pub fn baralho() -> Vec<Carta> {
    let mut v = Vec::with_capacity(40);
    for naipe in Naipe::TODOS {
        for valor in Valor::TODOS {
            v.push(Carta::nova(valor, naipe));
        }
    }
    v
}

#[cfg(test)]
mod testes {
    use super::*;
    use std::collections::HashSet;

    /// O exemplo literal do enunciado: 🂡 🂱 🃁 🃑 são os quatro ases.
    #[test]
    fn ases_do_enunciado() {
        assert_eq!(Carta::nova(Valor::As, Naipe::Espadas).unicode(), '🂡');
        assert_eq!(Carta::nova(Valor::As, Naipe::Copas).unicode(), '🂱');
        assert_eq!(Carta::nova(Valor::As, Naipe::Ouros).unicode(), '🃁');
        assert_eq!(Carta::nova(Valor::As, Naipe::Paus).unicode(), '🃑');
    }

    #[test]
    fn baralho_tem_40_cartas_distintas_com_40_caracteres_distintos() {
        let b = baralho();
        assert_eq!(b.len(), 40, "R-01: o baralho do truco tem 40 cartas");
        assert_eq!(b.iter().collect::<HashSet<_>>().len(), 40, "cartas repetidas");
        let chars: HashSet<char> = b.iter().map(|c| c.unicode()).collect();
        assert_eq!(chars.len(), 40, "duas cartas colidiram no mesmo caractere");
    }

    #[test]
    fn ida_e_volta_unicode_para_todas_as_40() {
        for c in baralho() {
            assert_eq!(Carta::do_unicode(c.unicode()), Some(c), "falhou em {c}");
        }
    }

    /// A armadilha do bloco Unicode: o Cavaleiro mora entre o Valete e a Dama, e
    /// 8/9/10 existem no bloco mas não no truco. Nenhum pode entrar.
    #[test]
    fn recusa_cavaleiro_e_oito_nove_dez() {
        for cp in [0x1F0AC, 0x1F0BC, 0x1F0CC, 0x1F0DC] {
            let c = char::from_u32(cp).unwrap();
            assert_eq!(Carta::do_unicode(c), None, "cavaleiro {c} entrou no baralho");
        }
        for base in [0x1F0A0u32, 0x1F0B0, 0x1F0C0, 0x1F0D0] {
            for d in [0x8, 0x9, 0xA] {
                let c = char::from_u32(base + d).unwrap();
                assert_eq!(Carta::do_unicode(c), None, "{c} nao existe no truco");
            }
        }
        // Coringas e cobertas do bloco também não.
        for cp in [0x1F0A0u32, 0x1F0BF, 0x1F0CF, 0x1F0DF] {
            if let Some(c) = char::from_u32(cp) {
                assert_eq!(Carta::do_unicode(c), None, "{c} nao e carta do truco");
            }
        }
    }

    /// R-02, e o ponto contraintuitivo: Q é mais fraca que J.
    #[test]
    fn ordem_das_cartas_comuns_com_dama_abaixo_do_valete() {
        // vira 4 => manilha é 5, então nenhuma destas é manilha
        let vira = Carta::nova(Valor::Quatro, Naipe::Paus);
        let ordem = [
            Valor::Quatro,
            Valor::Cinco,
            Valor::Seis,
            Valor::Sete,
            Valor::Dama,
            Valor::Valete,
            Valor::Rei,
            Valor::As,
            Valor::Dois,
            Valor::Tres,
        ];
        for par in ordem.windows(2) {
            let (a, b) = (par[0], par[1]);
            if a == Valor::Cinco || b == Valor::Cinco {
                continue; // 5 é manilha nesta vira
            }
            let fa = Carta::nova(a, Naipe::Ouros).forca(vira);
            let fb = Carta::nova(b, Naipe::Ouros).forca(vira);
            assert!(fa < fb, "R-02: {} devia ser mais fraca que {}", a.rotulo(), b.rotulo());
        }
        let dama = Carta::nova(Valor::Dama, Naipe::Paus).forca(vira);
        let valete = Carta::nova(Valor::Valete, Naipe::Ouros).forca(vira);
        assert!(dama < valete, "F-01: a Q e mais fraca que o J, em qualquer naipe");
    }

    /// R-02: entre cartas comuns o naipe não conta — e isso produz empate.
    #[test]
    fn carta_comum_empata_independente_do_naipe() {
        let vira = Carta::nova(Valor::Quatro, Naipe::Paus);
        let a = Carta::nova(Valor::Tres, Naipe::Paus).forca(vira);
        let b = Carta::nova(Valor::Tres, Naipe::Ouros).forca(vira);
        assert_eq!(a, b, "R-02: o naipe nao desempata carta comum");
    }

    /// R-03, incluindo a circularidade que F-01 avisa explicitamente.
    #[test]
    fn manilha_e_o_valor_seguinte_e_a_ordem_e_circular() {
        // "se uma carta '5' for a vira da rodada, as manilhas serão os '6'" — F-01
        let vira = Carta::nova(Valor::Cinco, Naipe::Ouros);
        assert!(Carta::nova(Valor::Seis, Naipe::Ouros).e_manilha(vira));
        // "se a vira for um 'J', as manilhas serão os 'K'" — F-01
        let vira = Carta::nova(Valor::Valete, Naipe::Copas);
        assert!(Carta::nova(Valor::Rei, Naipe::Paus).e_manilha(vira));
        // "quando a vira for o '3', as manilhas são as cartas '4'" — F-01
        let vira = Carta::nova(Valor::Tres, Naipe::Espadas);
        assert!(Carta::nova(Valor::Quatro, Naipe::Ouros).e_manilha(vira));
        // E a Q vem depois do 7, não depois do J.
        let vira = Carta::nova(Valor::Sete, Naipe::Ouros);
        assert!(Carta::nova(Valor::Dama, Naipe::Ouros).e_manilha(vira));
    }

    /// R-03: a manilha bate o 3, que é a carta comum mais forte.
    #[test]
    fn qualquer_manilha_bate_qualquer_carta_comum() {
        for vira in baralho() {
            let manilha_valor = vira.valor.proximo();
            for naipe_m in Naipe::TODOS {
                let m = Carta::nova(manilha_valor, naipe_m).forca(vira);
                for c in baralho() {
                    if c.e_manilha(vira) {
                        continue;
                    }
                    assert!(m > c.forca(vira), "manilha {} nao bateu {} (vira {})",
                        Carta::nova(manilha_valor, naipe_m), c, vira);
                }
            }
        }
    }

    /// R-04 e sua consequência: duas manilhas nunca empatam.
    #[test]
    fn ordem_dos_naipes_entre_manilhas_e_ouros_espadas_copas_paus() {
        let vira = Carta::nova(Valor::Sete, Naipe::Ouros); // manilha = Q
        let f = |n| Carta::nova(Valor::Dama, n).forca(vira);
        assert!(f(Naipe::Ouros) < f(Naipe::Espadas));
        assert!(f(Naipe::Espadas) < f(Naipe::Copas));
        assert!(f(Naipe::Copas) < f(Naipe::Paus));
        let todas: HashSet<Forca> = Naipe::TODOS.into_iter().map(f).collect();
        assert_eq!(todas.len(), 4, "R-04: duas manilhas nunca podem empatar");
    }

    /// R-12: a carta encoberta não vale nada — perde de tudo que está na mesa.
    #[test]
    fn carta_encoberta_perde_de_qualquer_carta() {
        for vira in baralho() {
            for c in baralho() {
                assert!(
                    c.forca(vira) > FORCA_ENCOBERTA,
                    "{c} deveria bater uma carta encoberta"
                );
            }
        }
    }

    /// A carta no JSON é o caractere, não um objeto nem um código.
    #[test]
    fn json_da_carta_e_o_proprio_caractere() {
        let c = Carta::nova(Valor::As, Naipe::Paus);
        assert_eq!(serde_json::to_string(&c).unwrap(), "\"🃑\"");
        assert_eq!(serde_json::from_str::<Carta>("\"🃑\"").unwrap(), c);
        // e o que não é carta é recusado, não silenciosamente aceito
        assert!(serde_json::from_str::<Carta>("\"AS\"").is_err());
        assert!(serde_json::from_str::<Carta>("\"🂬\"").is_err(), "cavaleiro");
        assert!(serde_json::from_str::<Carta>("\"🂡🂱\"").is_err(), "duas cartas");
        assert!(serde_json::from_str::<Carta>("\"\"").is_err());
    }
}
