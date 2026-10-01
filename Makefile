# Dois comandos é o que o enunciado pede: um que sobe tudo, um que testa.
.PHONY: subir derrubar testar e2e tudo logs limpar

subir:            ## sobe o jogo em http://localhost:8411
	docker compose up -d --build
	@echo "truco paulista em http://localhost:8411"

derrubar:
	docker compose --profile teste down -v

testar:           ## testa o servidor: 40 unidade + 7 integracao
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test

e2e:              ## testa o front num navegador real, em container
	docker compose --profile teste run --rm e2e

tudo: subir testar e2e  ## sobe e roda tudo

logs:
	docker compose logs -f truco

limpar:
	cargo clean && docker compose --profile teste down -v
