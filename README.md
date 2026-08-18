# Algedi

Cliente de sincronização de arquivos para GNOME, com suporte a Google Drive e
Microsoft OneDrive. Parte do ecossistema **Lyra OS**.

Algedi é composto por um daemon (`algedid`) que fala D-Bus na sessão do
usuário e por uma extensão do Nautilus que consome esse D-Bus para mostrar
selos de status e ações de sincronização no menu de contexto dos arquivos. A
configuração visual (contas, pastas sincronizadas) fica a cargo de um
aplicativo irmão, o Vega.

## Arquitetura

- `crates/algedid` — daemon binário. Expõe o serviço D-Bus
  `org.lyraos.Algedi1` (`/org/lyraos/Algedi1`), gerencia contas, credenciais
  (via Secret Service) e o agendador do ciclo de sincronização.
- `crates/algedi-core` — motor de sincronização (decisão de ações a partir de
  hashes locais/remotos, aplicação dessas ações, banco de estado SQLite),
  sem I/O de rede.
- `crates/algedi-provider-trait` — trait `CloudProvider`, comum aos adaptadores
  de provedor.
- `crates/algedi-provider-gdrive` — adaptador para Google Drive (OAuth2 +
  PKCE, chamadas à API do Drive).
- `crates/algedi-provider-onedrive` — adaptador para Microsoft OneDrive
  (OAuth2 + PKCE, chamadas ao Microsoft Graph).
- `nautilus-extension/` — extensão Python do Nautilus (selos de status e
  itens de menu), via `python313-nautilus`.
- `data/` — interface D-Bus, unit systemd `--user`, ícones de status.
- `packaging/` — spec RPM (`algedid` + subpacote `nautilus-algedi`).
- `docs/` — guias, incluindo o passo a passo de cadastro OAuth em
  `docs/oauth-setup.md`.

## Build

```sh
cargo build --workspace
cargo test --workspace
```

## Status

Em desenvolvimento ativo. Consulte `docs/oauth-setup.md` para o estado atual
da implementação e limitações conhecidas.

## Licença

Distribuído sob a [GNU General Public License v3.0](LICENSE) (ou, a seu
critério, qualquer versão posterior).
