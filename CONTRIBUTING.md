# Convenções do repositório

## Branches

- `main`: sempre estável. Não recebe commits diretos de trabalho em andamento.
- Trabalho em branches curtas, criadas a partir de `main`:
  - `feat/<descricao>`: nova funcionalidade
  - `fix/<descricao>`: correção de bug
  - `docs/<descricao>`: documentação
  - `refactor/<descricao>`: refatoração sem mudança de comportamento
  - `perf/<descricao>`: melhoria de desempenho
  - `test/<descricao>`: testes
  - `chore/<descricao>`: manutenção, build, dependências
- Nomes em kebab-case, curtos: `feat/dualsense-hid-reader`.
- Integração em `main` por merge local (`git merge --no-ff`) seguido de push. Sem Pull Requests:
  o projeto tem um único autor.

## Commits

Seguem [Conventional Commits](https://www.conventionalcommits.org/):

```
<tipo>(<escopo opcional>): <descrição no imperativo>

<corpo opcional>
```

Tipos: `feat`, `fix`, `docs`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `style`, `revert`.
Mudanças incompatíveis: `!` após o tipo/escopo (`feat(protocol)!: ...`) e rodapé `BREAKING CHANGE:`.

Exemplos:

```
feat(core): detectar conexão de DualSense via USB
fix(ui): corrigir indicador de bateria em Bluetooth
docs: adicionar spec de arquitetura
```

## Autoria

Commits e PRs têm um único autor. Não usar trailers `Co-Authored-By`
nem linhas de atribuição a ferramentas.
