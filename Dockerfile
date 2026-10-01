# Duas fases: compilar com a toolchain completa, servir com o mínimo.
# O binário final não carrega cargo, rustc nem fonte.

FROM rust:1-slim AS construtor
WORKDIR /src
# TLS por rustls (ver D-10), então não há dependência de libssl para instalar aqui.
# sqlx empacota o SQLite, então também não há libsqlite3.
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
# --locked: o container compila exatamente as versões do Cargo.lock, não "as mais novas hoje".
RUN cargo build --release --locked

FROM debian:stable-slim
# ca-certificates: para entregar webhook em https.
# fonts-noto-core: o bloco Unicode Playing Cards precisa de fonte, senão a carta vira tofu
#   em screenshot. Não afeta correção (o caractere está no DOM), afeta o que se vê.
# curl: só para o healthcheck ter como perguntar.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl fonts-noto-core \
 && rm -rf /var/lib/apt/lists/*

# Usuário sem privilégio: o processo não tem razão para ser root, e um servidor de jogo
# exposto é exatamente o lugar onde isso importa.
RUN useradd --system --create-home --uid 10001 truco
WORKDIR /app
COPY --from=construtor /src/target/release/truco /usr/local/bin/truco
COPY web ./web
# O banco mora em volume próprio, não no diretório da aplicação, que fica só de leitura.
RUN mkdir -p /dados && chown truco:truco /dados
USER truco
VOLUME ["/dados"]
ENV DATABASE_URL=sqlite:///dados/truco.db \
    PORTA=8080 \
    RUST_LOG=truco=info
EXPOSE 8080
HEALTHCHECK --interval=5s --timeout=3s --retries=10 --start-period=3s \
  CMD curl -fsS http://127.0.0.1:8080/api/saude || exit 1
CMD ["truco"]
