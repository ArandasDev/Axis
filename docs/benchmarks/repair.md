# Kit de correção: verificação com o autor

Data: 2026-11-10/11. Build release, núcleo na bandeja e interface conectados pelo pipe (protocolo v2).
Controles: dois DualShock 4 clones por Bluetooth (~198 Hz).

## DS4 `41:42:00:00:84:b6` ("Controle 1"): analógico esquerdo desgastado

- Parado, o eixo vertical chegou a ficar em **−0,38** (não volta ao centro sozinho) e oscila cerca de ±0,1.
- Primeira medição de drift (leigo): centro 0,00 · 0,04, ruído **0,13**, raio **0,19**, **marcado como desgastado** (|centro| + raio > 0,15) e aplicado. Com a correção, o jogo recebe 0,00 com o analógico solto.
- Medições seguintes foram recusadas ("mexeu") porque o analógico ainda estava voltando ao centro durante os 3 s. Ajuste feito depois do teste: o drift descarta o primeiro segundo (assentamento) e a recusa explica que, sem toque, isso é sinal de desgaste.

## DS4 `eb:67:4c:9a:fd:b4` ("Controle 2"): saudável

| Assistente | Resultado |
|---|---|
| Drift esquerdo (leigo) | centro 0,00 · 0,00, raio 0,03 (mínimo), sem aviso |
| Alcance esquerdo | raios 0,93 a 1,01 |
| Alcance direito (medido duas vezes) | raios 0,87 a 1,01; a diagonal de baixo à esquerda fica curta nas duas medições → característica do controle; erro de circularidade ~6% |
| Gatilhos | L2 e R2: repouso 0,01, máximo 0,99 |
| Conexão | Bluetooth, 198 Hz, jitter p99 ~6,2 ms, 0 desconexões, bateria 30% |

- **Persistência:** a correção continuou aplicada depois de reiniciar o núcleo (lida de `controllers/<id>.toml`).
- Latência p99 do núcleo durante o teste: 30–90 µs.
- O cartão de cada assistente atualizou o texto na tela real (o atraso de redesenho visto só nas capturas `PrintWindow` não apareceu no monitor).

## Ajustes feitos durante a verificação

- Drift espera 1 s para o analógico assentar; mensagem de recusa explica desgaste (`1db190f`).
- Malha da grade não vaza mais nos cantos arredondados dos cartões (o renderizador de software recorta só em retângulo) (`dc7124f`).
- Atalho "Axis" criado na Área de Trabalho do autor (fora do repositório).

## Pendências

- Redimensionar a janela por fora (Snap, Win+setas, `SetWindowPos`) derruba a interface no renderizador de software do Slint (anterior a este plano).
- Com a janela maximizada, o gráfico do assistente fica pequeno e sobra espaço no painel.
- Contagem de apertos por gatilho durante a medição (o protocolo só leva o progresso combinado).
