// A versão aqui tem de casar com a da imagem do compose (v1.56.0-noble): a imagem traz os
// navegadores daquela versão, e `@playwright/test` de outra versão não os reconhece.
const { defineConfig, devices } = require('@playwright/test');

module.exports = defineConfig({
  testDir: '.',
  // Um worker: os testes compartilham um servidor com banco único, e o pareamento junta
  // quem quer a mesma aposta. Dois workers em paralelo sentariam jogadores de testes
  // diferentes na MESMA mesa — e aí o teste mediria a interferência, não o jogo.
  workers: 1,
  fullyParallel: false,
  retries: 0,
  timeout: 90_000,
  expect: { timeout: 15_000 },
  reporter: [['list']],
  use: {
    baseURL: process.env.BASE_URL || 'http://localhost:8411',
    headless: true,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    // O jogo é em português e as cartas são caracteres fora do BMP: a localidade importa
    // para o navegador escolher fonte, e o teste existe em parte para exercitar isso.
    locale: 'pt-BR',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
});
