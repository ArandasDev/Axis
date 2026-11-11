# Checklist de hardware

Testes que dependem de um controle físico ficam marcados `#[ignore = "hardware: …"]` e rodam com:

```bash
cargo test -p axis-devices -- --ignored
```

Um controle passa a ser **verificado** (e perde o selo "experimental" na interface) quando todos os itens da sua seção estão marcados. Ver spec 4.1.1.

## DualShock 4

- **Bluetooth: verificado em 2026-11-09** (clone, MAC `41:42:00:00:84:b6`)
- **USB: pendente** (o cabo micro-USB disponível é só de carga)

| Item | Bluetooth | USB |
|---|---|---|
| Enumera e abre (`ds4_is_enumerated_opened_and_reads_reports`) | ✅ | ☐ |
| ID é o MAC de 17 caracteres, estável entre reconexões | ✅ (serial) | ☐ (feature `0x12`) |
| Lê ≥ 40 de 50 leituras | ✅ | ☐ |
| Modo completo `0x11` ativado pelo feature `0x05` | ✅ | n/a |
| Sticks e gatilhos chegam aos extremos (`real_ds4_bt_fixture_covers_extremes`) | ✅ | ☐ |
| Botões: ✕ ○ □ △, d-pad, L1, R1, Share, Options, PS, L3, R3 | ✅ | ☐ |
| Clique do touchpad | ☐ (não exercitado na gravação) | ☐ |
| Lightbar muda de cor (`ds4_lightbar_turns_green_then_back`) | ✅ (confirmado visualmente) | ☐ |
| Vibração | ☐ | ☐ |
| Bateria e carregamento | ☐ (lida 50% no report `0x11`; não conferida com o real) | ☐ |
| Desconexão gera `DeviceError::Disconnected` | ✗ não acontece: desligar no Bluetooth deixa o handle mudo (sem erro) e ele volta a funcionar ao religar; o núcleo detecta 500 ms de silêncio (verificado em 2026-11-10) | ☐ |
| Desligar/religar com o núcleo: "reconectou" na graça, virtual liberado além dela | ✅ 2026-11-10 | ☐ |
| Hotplug detecta remoção e chegada (`arrival_and_removal_are_reported`) | ✅ | ☐ |

**Pontos de atenção encontrados:**
- O Windows entrega os reports Bluetooth com **128 bytes** (preenchidos). Um report reduzido `0x01` (que aparece antes do feature `0x05`, ou em clones que ficam no modo reduzido) passaria do limite de 31 bytes e mostraria **bateria 0%** a partir do preenchimento. Tratar no plano 4 (ignorar bateria no `0x01` vindo do Bluetooth).
- Segundo DS4 do autor lido em 2026-11-11: MAC `eb:67:4c:9a:fd:b4` (diferente do primeiro, sem colisão). O terceiro ainda não. Se dois clones informarem o mesmo MAC, as identidades colidem (o código só rejeita MACs todo-zero e todo-`FF`).
- Kit de correção (plano 5b) verificado em 2026-11-11 com os dois DS4 por Bluetooth: drift, alcance, gatilhos, conexão e persistência após reiniciar o núcleo. Detalhes em `docs/benchmarks/repair.md`. O analógico esquerdo do DS4 `41:42:…` não volta ao centro sozinho (desgaste); o assistente o marca como desgastado.

## DualSense (experimental)

- **Estado: pendente** (sem controle disponível)

| Item | Bluetooth | USB |
|---|---|---|
| Enumera e abre | ☐ | ☐ |
| ID pelo feature `0x09` (USB) / serial (BT) | ☐ | ☐ |
| Modo completo `0x31` ativado pelo feature `0x05` | ☐ | n/a |
| Sticks, gatilhos e botões conforme o layout documentado | ☐ | ☐ |
| Bateria (`d[52]`) | ☐ | ☐ |
| Lightbar (pode exigir `valid_flag2` / "light out" no primeiro envio) | ☐ | ☐ |
| LEDs de jogador (padrões 1–4) | ☐ | ☐ |
| Vibração | ☐ | ☐ |
| Layout do report de saída BT (payload no byte 2, como no SDL) | ☐ | n/a |
| Hotplug | ☐ | ☐ |

## Xbox (experimental, via XInput)

- **Estado: pendente** (sem controle disponível)

| Item | Resultado |
|---|---|
| `xbox_controller_reads_state` | ☐ |
| Sticks, gatilhos e botões mapeados | ☐ |
| Vibração | ☐ |
| Slots dos controles virtuais do Axis são excluídos | ☐ (precisa do daemon, plano 4) |
| `transport()` reporta Bluetooth quando for o caso (hoje fixo em USB) | ☐ |
