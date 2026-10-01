# `cartas.woff2`

Subconjunto de **Noto Sans Symbols 2** (Noto Project Authors, licença SIL Open Font License 1.1)
contendo **apenas os 41 glifos** que este jogo desenha: as 40 cartas do truco paulista
(`U+1F0A1`–`U+1F0DE`, pulando 8, 9, 10 e o Cavaleiro) mais o dorso `🂠 U+1F0A0`.

## Por que a fonte está embarcada

Sem ela, o jogo fica ilegível em boa parte das máquinas. O bloco Unicode *Playing Cards* é
coberto por poucas fontes de sistema: o macOS tem (Apple Symbols), a maioria das instalações de
Linux e Windows não. **Eu descobri isto numa captura de tela do próprio jogo**, rodando no
container do Playwright: as cartas apareciam como caixas com o código hexadecimal dentro.

O requisito do enunciado é que a carta **trafegue** como o caractere Unicode, e isso vale no
protocolo com ou sem fonte. Mas "jogável" inclui o jogador conseguir ler a carta, e uma mesa de
truco cheia de caixinhas não é jogável. Então a fonte vem junto.

## Por que um subconjunto, e não a fonte inteira

A Noto Sans Symbols 2 completa tem **671 KB**. O subconjunto tem **8,6 KB** — 1,3% do tamanho —
porque carrego 41 glifos em vez de alguns milhares. Num jogo em tempo real, 660 KB de fonte
atrasariam a primeira mesa sem desenhar nada a mais.

## Como foi gerado

```
pyftsubset NotoSansSymbols2-Regular.ttf \
  --unicodes='U+1F0A0,U+1F0A1,...,U+1F0DE' \
  --flavor=woff2 --no-hinting --desubroutinize \
  --output-file=cartas.woff2
```

O `pyftsubset` rodou num container `python:3-slim` descartável, não no host: a regra de nunca
instalar nada no sistema vale também para ferramenta de build de asset.

Fonte original: <https://github.com/notofonts/symbols> · Copyright 2022 The Noto Project
Authors, sob OFL 1.1 — que permite redistribuição, inclusive subconjunto.
