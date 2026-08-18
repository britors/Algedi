Name:           algedi
Version:        0.1.0
Release:        0
Summary:        Cliente de sincronizacao de arquivos para Google Drive e OneDrive
License:        GPL-3.0-or-later
URL:            https://github.com/lyraos/algedi
Source0:        %{name}-%{version}.tar.xz

BuildRequires:  rust
BuildRequires:  cargo
BuildRequires:  pkgconfig(sqlite3)
BuildRequires:  pkgconfig(libsecret-1)
BuildRequires:  systemd-rpm-macros

Requires:       xdg-utils

%description
Algedi sincroniza pastas locais com Google Drive e Microsoft OneDrive de
forma bidirecional, com suporte a multiplas contas e integracao com o
Nautilus. A configuracao (contas, pares de pastas, preferencias) e feita
pelo Vega; este pacote fornece apenas o daemon de sincronizacao e a
integracao com o Nautilus. Parte do ecossistema Lyra OS.

%package -n nautilus-algedi
Summary:        Integracao do Algedi com o Nautilus (Files)
Requires:       %{name} = %{version}-%{release}
Requires:       python313-nautilus
BuildArch:      noarch

%description -n nautilus-algedi
Extensao Python que adiciona emblemas de status e menu de contexto do
Algedi ao Nautilus, consumindo o servico D-Bus org.lyraos.Algedi1.

%prep
%autosetup

%build
cargo build --release --workspace

%install
install -Dm755 target/release/algedid %{buildroot}%{_libexecdir}/algedi/algedid

install -Dm644 data/org.lyraos.Algedi1.xml \
    %{buildroot}%{_datadir}/dbus-1/interfaces/org.lyraos.Algedi1.xml
install -Dm644 data/org.lyraos.algedid.service \
    %{buildroot}%{_userunitdir}/org.lyraos.algedid.service

install -Dm644 nautilus-extension/algedi_nautilus.py \
    %{buildroot}%{_datadir}/nautilus-python/extensions/algedi_nautilus.py

%post
%systemd_user_post org.lyraos.algedid.service

%preun
%systemd_user_preun org.lyraos.algedid.service

%files
# TODO: add %%license once a LICENSE file is committed to the repo root.
%{_libexecdir}/algedi/algedid
%{_datadir}/dbus-1/interfaces/org.lyraos.Algedi1.xml
%{_userunitdir}/org.lyraos.algedid.service

%files -n nautilus-algedi
%{_datadir}/nautilus-python/extensions/algedi_nautilus.py

%changelog
* Tue Aug 18 2026 Lyra OS <packaging@lyraos.org> - 0.1.0-0
- Scaffold inicial do pacote Algedi.
