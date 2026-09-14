# Native Voice boundary — candidato offline

Este pacote ESM implementa contratos e fences locais. O único adapter entregue é FAKE, sem dispositivo, rede, IPC, processos ou código do Desktop. Não é um plugin instalado nem prova do hook nativo, V3 onset real ou recuperação do Windows.

## API e integração com o produtor

`createNativeVoiceBoundary({adapter,timeoutMs})` expõe `beginSession`, `playNativeOutput`, `speechOnset`, `closeVoice`, `gateVaiJob`, `snapshot` e `events`. O contrato TypeScript está em `index.d.ts`.

Scope completo: `{threadId,nativeSessionId,voiceGeneration,ownerId}`. Uma geração maior só começa depois de CLOSED da anterior na mesma thread; trocar owner exige geração maior. Não transferir ownership de Residence/produção a partir deste campo: ele identifica apenas o owner do transporte de voz candidato.

`toProducerScope` faz o mapeamento explícito para `{thread_id,native_session_id,voice_session_generation,owner_id}`. `jobGeneration` é outro número: fence local de jobs, avançado por onset/close; não deve substituir `voice_session_generation`.

`gateVaiJob` é condição necessária, nunca autorização suficiente. Mesmo `allowed=true` retorna `finalProvenanceAuthorized=false`. Receipt confiável de ingresso, final backend e autorização do egress continuam pertencendo ao produtor. O executor deve revalidar a geração imediatamente antes de iniciar seu próprio player; ler um boolean antes do TTS não reserva permissão futura.

## Invariantes implementadas

1. `beginSession` solicita exclusivamente setOutputMuted(true), exige OUTPUT_MUTED_ACK com scope, operationId, muted=true e observação SINK_READBACK. ACK de intenção, false, ausente, owner/geração/operação velhos não liberam play.
2. `playNativeOutput` só chama o sink após o ACK. Isso requer que o adapter detenha o sink **desde a criação**, antes de autoplay/ontrack. Envolver um elemento que já reproduziu não prova ausência da primeira sílaba.
3. Mute não toca capture/microfone, peer, data channel ou subscrições Core. O adapter de teardown recebe somente o scope Voice a aposentar; o boundary não chama turn/interrupt nem cancela o trabalho backend.
4. Onset avança jobGeneration antes de qualquer await; o gate bloqueia tanto o job antigo quanto novo playback até stop/invalidação confirmados. Eventos são deduplicados por eventId dentro do scope. Dois onsets concorrentes preservam o número aceito em cada receipt.
5. `stopPlayer` deve atuar somente em handles adquiridos pelo controlador, do scope pedido, com geração menor que `nextJobGeneration`. Nunca procurar/matar player global ou processo por nome.
6. Close revoga o gate e avança o fence antes de I/O. Invalida jobs e solicita stop em paralelo; depois aposenta Voice. Só reconcilia apresentação após passos prévios confirmados. VOICE_CLOSE_ACK exige transporte aposentado, owner Voice liberado, thread e subscrições backend preservados. PRESENTATION_ACK deve informar `composerUsable:boolean` e `independentBlockers` medidos. Sem blocker, false mantém HOLD; campo ausente não vira pronto. Os únicos blockers reconhecidos são `backendBusy`, `permissionPrompt` e `threadReadOnly`: estados independentes de Voice, nunca `voiceStillReserved`. Um blocker legítimo permite CLOSED da voz com `composerReady=false`, preservando no receipt a indisponibilidade e sua causa. Não forçar idle nem apagar trabalho real.
7. Timeout/falha é HOLD, sem retry produtivo nem reabertura automática. Close tardio da geração N não envia operações para N+1; ACK de preparação velho não pode reabrir N. Replay de close não repete teardown.

## Seam nativo exato proposto — ainda não conectado

Fonte pública estática: `stvlynn/codex-desktop@bdd7ceaf144ceb9c5f5a363a5ea5ce6963ac8fbc`, package 26.721.31836/build5828. Referência anterior congelada: `voice-real-chain-20260914/frontend/FRONTEND-SOURCE-EVIDENCE.json`. Não afirmar equivalência com Windows 26.908.4834.0.

No [app-initial raw](https://github.com/stvlynn/codex-desktop/blob/bdd7ceaf144ceb9c5f5a363a5ea5ce6963ac8fbc/ref/webview/assets/app-initial-C-fROkKo.js):

- L510704–510740: `sns.start` cria audioElement, define autoplay/hidden/muted e registra ontrack. O hookup deve impor muted=true desde este ponto e conferir o mesmo elemento antes de chamar play. Não é suficiente trocar output_modality.
- L510757–510766: setOutputAudioMuted altera somente audioElement.muted; input possui setter separado.
- L510864–510878 e L512232–512270: runtime.setOutputMuted e controle interno set-output-muted; o adapter deve ler de volta muted no sink da geração atual. O comando UI de toggle não é idempotente.
- L511029–511056: handler data channel reconhece session usage e session initialization, não exporta speech_started. O candidato **não fabrica** um onset V3 real.

No [main raw](https://github.com/stvlynn/codex-desktop/blob/bdd7ceaf144ceb9c5f5a363a5ea5ce6963ac8fbc/ref/.vite/build/main-D9i1FeCI.js), L104516–104559 toggle/wake retorna quando reserved; close pode resetar lançamento e fechar renderer. Isso não constitui API determinística externa. Não usar toggle pet como implementação de reconcilePresentation. O adapter real necessita operação scoped/ack no owner e só remover referências de apresentação da geração encerrada, preservando draft/thread/backend.

As URLs são alvos de desenho, não um patch válido de bundle minificado. Nenhum bundle é importado, executado ou modificado por estes testes. Não expor eval/IPC arbitrário nem procurar objeto privado global em runtime.

## Limites materiais

- Estado/journal e dedup em memória, um processo Node. Não há prova de persistência entre crashes, fence multi-processo ou autoridade central. Após restart não restaurar READY de um dump: exigir nova reconciliação e geração fornecida pelo owner confiável.
- O adapter é uma fronteira de confiança. Além de devolver ACK correto, deve conferir scope/owner **imediatamente antes** de cada efeito assíncrono. Um ACK não autentica um adapter malicioso. O candidato não consegue desfazer uma mutação indevida já feita pelo adapter.
- Fechar Voice pode desativar captura daquela sessão, por seu teardown autorizado. A garantia de preservar mic/transport nesta fatia refere-se ao mute de saída; close preserva thread/trabalho/subscrições backend, não mantém a sessão Voice fechada capturando.
- A garantia pré-primeira-amostra depende do hook anterior à criação/play. Não há hookup instalado entregue. Onset real V3, dispositivo, latência, barge-in audível e composer Windows são NC.
- `NATIVE_PRIVATE_SEAM` é tipo para integração futura. Os testes e recibos usam apenas `FAKE`/`FAKE_FIXTURE`. Ele não significa capability ativa ou autorização de executar Voice.

## Provas reproduzíveis

Dentro deste diretório, sem dependências externas:

```sh
node --test test-native-boundary.mjs
node test-native-boundary.mjs
FDV_BOUNDARY_TEST_NEGATIVE=1 node --test test-native-boundary.mjs
```

O último comando deve falhar por INTENTIONAL_NEGATIVE_CONTROL. Nesta versão Node20 a invocação `--test` apresentou resumo por arquivo; execução direta do mesmo node:test revelou os 30 cenários. O controle negativo prova que o arquivo não foi aceito sem executar assertions. `run-proof.py` salva stdout/stderr, rc e hashes nesta pasta em uma subpasta nova, sem tocar fontes externas. Resultados sintéticos não são prova de áudio real.

Correção após revisão: a primeira bateria de 25 cenários exigia apenas detach/preservação no ACK, insuficiente para declarar composer utilizável. Esses recibos permanecem preservados; os seis arquivos anteriores estão congelados em `pre-composer-ack/`, somente evidência, não entrada do runner. A bateria atual acrescenta cinco cenários da pós-condição do composer, incluindo ausência de campos, falso pronto e backend ocupado legítimo. O hookup Windows continua NC.


## Delta após revisão Sol — gate combinado

`authorizeVaiPlayback({scope, readProducerEvidence})` é a única decisão combinada
`READY_FOR_TTS`. `gateVaiJob` continua um fence necessário, sem authority de áudio.
O método exige readback ativo do adapter da MESMA sessão nativa e owner, sink muted,
prova atual do produtor relida depois do await e todos os joins thread/session/voice
 generation/origin/client/turn/final/job generation. Não aceita um boolean PASS.

`readProducerEvidence` deve ser um reader síncrono confiável da projeção corrente de
fonte+journal, equivalente a `final_producer.read_playback_evidence` no domínio que
detém essa leitura. O callback real entre Python e Desktop NÃO está implementado;
nestes testes ele fornece fixtures extraídas de SQLite sintético. Hash de snapshot
é integridade, não autenticação de autor/owner. O readback native também é FAKE.

Onset/close durante o await e durante reader reentrante são novamente confrontados
antes da decisão. Uma geração de job antiga não pode ser renovada por autorização
nova. Os 93 thread/turn históricos são bloqueados novamente por ledger SHA fixo.

Esta candidata não chama TTS nem player VAI. `egressAuthorized=false` e a decisão
não é token reutilizável. O futuro consumer deve invocar esse gate imediatamente
antes do efeito e novamente ao receber TTS/master atrasado; adapter deve fencear
scope antes de cada efeito. Não há alegação de atomicidade entre processos ou
Desktop, nem de autenticação de packet JSON arbitrário fornecido por caller.

Novo método obrigatório do adapter: `observeLiveSession` retorna `LIVE_SESSION_ACK`
com request operationId/scope, active=true, ownerCurrent=true, outputMuted=true e
observation=SESSION_AND_SINK_READBACK. Sua implementação instalada permanece NC.

`run-proof.py` mede 30 cenários da boundary anterior e 30 do gate combinado, com
controles negativos de ambos os runners e contagens TAP explícitas. O stdout do
`node --test` nesta bancada resume um arquivo; não é usado como contagem de casos.
A declaração TypeScript anterior teve PASS externo informado pela Sol; o delta
novo ainda precisa de tsc. Nenhum Windows/Voice/ElevenLabs nesta rodada.
