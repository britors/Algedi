# Configurando credenciais OAuth (Google Drive e OneDrive)

O `algedid` não vem com nenhuma credencial embutida. Antes de `AddAccount`
funcionar para um provedor, você precisa registrar um app OAuth nesse
provedor e colocar as credenciais em `providers.toml` (ou em variáveis de
ambiente). Sem isso, `AddAccount` falha com uma mensagem de erro clara
dizendo exatamente o que falta — não trava nem faz nada silenciosamente.

## 1. Google Drive

1. Acesse o [Google Cloud Console](https://console.cloud.google.com/) e crie
   (ou selecione) um projeto.
2. **APIs e serviços → Biblioteca** → procure "Google Drive API" → **Ativar**.
3. **APIs e serviços → Tela de consentimento OAuth**:
   - Tipo de usuário: **Externo** (a menos que você tenha um Google Workspace
     e queira restringir ao seu domínio, aí use **Interno**).
   - Preencha nome do app ("Algedi"), e-mail de suporte, e-mail do
     desenvolvedor.
   - Em **Escopos**, adicione `.../auth/drive.file` (e opcionalmente
     `.../auth/drive`, usado como opção avançada — ver PROMPT-ALGEDI.md §3).
   - Se o app ficar em modo "Teste" (comum, evita o processo de verificação
     do Google), adicione sua própria conta Google em **Usuários de teste** —
     sem isso o login vai ser recusado.
4. **APIs e serviços → Credenciais → Criar credenciais → ID do cliente OAuth**:
   - Tipo de aplicativo: **App para computador** ("Desktop app").
   - Dê um nome (ex: "Algedi Desktop") e clique em **Criar**.
5. O Google mostra um **ID do cliente** (`client_id`, termina em
   `.apps.googleusercontent.com`) e uma **Chave secreta do cliente**
   (`client_secret`). Copie os dois.

   > Apps "Desktop" do Google recebem um `client_secret`, mas ele não é
   > tratado como confidencial nesse fluxo (não há como um app desktop
   > escondê-lo do usuário) — a segurança real vem do PKCE, que o Algedi já
   > usa. Ainda assim, trate o arquivo `providers.toml` como sensível.

   Você **não precisa** cadastrar a porta do redirect: clientes tipo
   "Desktop app" do Google aceitam automaticamente qualquer
   `http://127.0.0.1:<porta>/...` de loopback.

## 2. Microsoft OneDrive (Microsoft Entra ID / Azure AD)

1. Acesse o [portal do Azure](https://portal.azure.com/) → **Microsoft Entra
   ID → Registros de aplicativo → Novo registro**.
2. Nome: "Algedi".
3. **Tipos de conta com suporte**: escolha **Contas em qualquer diretório
   organizacional e contas pessoais da Microsoft** — o Algedi usa o tenant
   `common` (PROMPT-ALGEDI.md §3), que aceita os dois.
4. Em **URI de redirecionamento**, escolha a plataforma
   **Aplicativos móveis e para desktop** e adicione exatamente:
   ```
   http://localhost
   ```
   **Sem porta, sem caminho.** Essa é a única forma pela qual a Microsoft
   identity platform aceita qualquer porta de loopback nesse tipo de
   cliente — é por isso que o Algedi usa `localhost`, não `127.0.0.1`, no
   redirect do OneDrive (diferente do Google Drive).
5. Clique em **Registrar**. Copie o **ID do aplicativo (cliente)** — esse é
   o `client_id`. Registros desse tipo (cliente público) **não usam**
   `client_secret`; o campo fica vazio de propósito no `providers.toml`.
6. **Permissões de API → Adicionar uma permissão → Microsoft Graph →
   Permissões delegadas**, adicione:
   - `Files.ReadWrite.All`
   - `offline_access`
7. **Autenticação → Configurações avançadas**: confirme que **Permitir
   fluxos de cliente público** está **Sim** (necessário para PKCE sem
   `client_secret`).

   > Se a autorização falhar com `AADSTS...redirect_uri_mismatch`, o motivo
   > quase certo é o passo 4 — confira que o valor cadastrado é exatamente
   > `http://localhost`, sem porta e sem barra final.

## 3. Preenchendo `providers.toml`

Crie o arquivo (o daemon já cria o diretório pai se precisar, mas o arquivo
em si você escreve à mão):

```
$XDG_CONFIG_HOME/algedi/providers.toml
```

Se `$XDG_CONFIG_HOME` não estiver definida, use `~/.config/algedi/providers.toml`.

Conteúdo:

```toml
[gdrive]
client_id = "SEU-ID.apps.googleusercontent.com"
client_secret = "SEU-CLIENT-SECRET"

[onedrive]
client_id = "11111111-2222-3333-4444-555555555555"
# onedrive não usa client_secret — deixe essa chave de fora.

[scheduler]
# Opcional; padrão 60 segundos, mínimo efetivo de 15 segundos.
poll_interval_secs = 60
```

Qualquer chave que faltar simplesmente fica indisponível para aquele
provedor (`AddAccount` retorna erro explicando o que configurar).

### Alternativa: variáveis de ambiente

Útil para testar sem escrever o arquivo, ou para overridar um valor
específico. Têm prioridade sobre o `providers.toml`:

```bash
export ALGEDI_GDRIVE_CLIENT_ID="SEU-ID.apps.googleusercontent.com"
export ALGEDI_GDRIVE_CLIENT_SECRET="SEU-CLIENT-SECRET"
export ALGEDI_ONEDRIVE_CLIENT_ID="11111111-2222-3333-4444-555555555555"
export ALGEDI_POLL_INTERVAL_SECS="60"
```

## 4. Testando

Com o `algedid` rodando (`cargo run -p algedid`, ou via systemd --user) e o
`providers.toml` preenchido:

```bash
gdbus call --session --dest org.lyraos.Algedi1 \
  --object-path /org/lyraos/Algedi1 \
  --method org.lyraos.Algedi1.AddAccount "gdrive"
```

Isso deve abrir o navegador padrão na tela de login do Google/Microsoft. Ao
autorizar, o navegador redireciona para `http://127.0.0.1:<porta>/...` (ou
`http://localhost:<porta>` no caso do OneDrive), o `algedid` captura o
código, troca por tokens, guarda no Secret Service (chaveiro do GNOME) e
retorna o `account_id` da conta recém-criada.

## Estado atual da implementação

- ✅ Captura do redirect loopback (`tiny_http`), troca de código por token,
  refresh de token, revogação, e persistência via Secret Service — tudo
  implementado e coberto por testes (veja `cargo test --workspace`).
- ✅ O scheduler renova automaticamente o `access_token` cinco minutos antes
  da expiração, persiste o novo conjunto de tokens no Secret Service e o
  instala atomicamente no adaptador antes de iniciar o próximo ciclo de sync.
