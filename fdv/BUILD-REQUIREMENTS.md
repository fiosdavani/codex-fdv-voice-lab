# Build pendente desta candidata

O pin oficial exige Rust **1.95.0** (`codex-rs/rust-toolchain.toml`). `cargo`, `rustc`, `rustfmt` e `just` não estão disponíveis no executor medido. `just fmt`, `just test -p codex-state` e `just fix -p codex-state` falharam antes de executar com rc=127. Nenhuma falha foi tratada como teste verde.

Git fetch do commit `6f39a47bb3b04de4c804187bfbf55edc56939aab` foi tentado normalmente e com aprovação focal; ambos falharam na resolução DNS de github.com. O conector GitHub disponível forneceu um **snapshot seletivo de 45 arquivos**, conferidos por Git blob SHA-1. O baseline Git local é um import desse conjunto, não o commit upstream completo. A candidata tem branch própria; o patch usa os bytes upstream reais como base.

Menor ambiente necessário para fechar a prova Rust: workspace descartável gravável, contendo checkout completo do mesmo commit, toolchain1.95.0 com rustfmt/clippy, `just` e o cache de dependências/build exigido pelo lockfile e pelas receitas existentes. Sem secrets, sem Residence operacional, sem socket privado, sem deploy. Não precisa dar root ou rede geral à tarefa; os artefatos/cache podem ser provisionados previamente por canal administrativo autorizado. Não há motivo para aumentar authority produtiva.

Verificação desse provisionamento: HEAD upstream exato, working tree inicialmente limpo, hashes dos arquivos-base e toolchain. Aplicar os patches **somente nesse checkout de desenvolvimento** e executar as receitas do AGENTS.md. Primeiros testes pertinentes: `just test -p codex-state`, `just test -p codex-thread-store`, `just test -p codex-queue-extension`, `just test -p codex-core` e testes do app-server/extension-api afetados. `just fmt`/`just fix` também pendentes. Suítes Core/App-server usam seus mocks, não endpoints produtivos. Uma futura execução deve respeitar a política de sockets da bancada e não silenciar skips/erros.

Risco: espaço/tempo de build e mudanças somente no candidate. Rollback: descartar essa cópia/branch de desenvolvimento; binário oficial e tarefa viva nunca são substituídos. Nenhum binário candidate foi gerado ou instalado nesta rodada.

Mesmo Rust verde não prova o adapter Desktop privado: conectar/atestá-lo, o produtor real de onset V3 e a emissão confiável de autorização/recibo é uma etapa distinta. Testes Python/Node daqui não substituem esses gates.
