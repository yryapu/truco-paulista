//! Truco paulista: servidor autoritativo.
//!
//! As regras do jogo e a origem de cada uma estão no repositório de pesquisa:
//! <https://github.com/yryapu/poliorketikos-truco-paulista>. Os comentários deste crate
//! citam os IDs `R-nn` (regra com fonte) e `D-nn` (decisão minha onde a fonte silencia).

pub mod api;
pub mod auth;
pub mod cartas;
pub mod db;
pub mod economia;
pub mod mesa;
pub mod regras;
pub mod webhooks;
