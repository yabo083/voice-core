# Third-party notices

voice-core is licensed under GPL-3.0-or-later (see `LICENSE`). This file records every
third-party component that ships inside a voice-core artefact, what licence it carries,
and why it is in the tree.

Two artefacts are distinguished throughout, because they contain very different things:

| Artefact | What it is | Contains |
|---|---|---|
| **source publication** | this repository | Rust and C# sources, docs, `worker/irodori/worker.py`, one bundled font |
| **portable package** | `scripts/package.ps1` output | the above, compiled, plus the Windows App SDK redistributables, and — with `-IncludeEngine` / `-IncludeModels` — a Python virtualenv, the Irodori-TTS engine source and model weights |

Licences below were read from the components themselves on this machine, not from memory:
crate manifests under `~/.cargo/registry/src/`, `*.nuspec` and `license.txt` under
`~/.nuget/packages/`, `*.dist-info/METADATA` and `*.dist-info/LICENSE` in the engine
virtualenv, and Hugging Face model cards. Anything that could not be resolved from a
file is marked **UNVERIFIED** rather than guessed.

---

## BLOCKER — Windows App SDK terms conflict with GPL-3.0

**This affects the portable package only, not the source publication.** It must be
resolved before any binary release is published. Evidence:

- `app/VoiceCoreTray/VoiceCoreTray.csproj:17` — `<WindowsAppSDKSelfContained>true</WindowsAppSDKSelfContained>`
- `app/VoiceCoreTray/VoiceCoreTray.csproj:22` — `Microsoft.WindowsAppSDK` 1.6.250108002
- `scripts/package.ps1:128` — `Copy-Tree $trayDir (Join-Path $outRoot 'bin\app')` copies the
  whole tray output directory, which on disk today contains ~40 Microsoft runtime DLLs
  (`Microsoft.ui.xaml.dll`, `Microsoft.WindowsAppRuntime.dll`, `DWriteCore.dll`,
  `Microsoft.UI.Composition.OSSupport.dll`, …)
- `~/.nuget/packages/microsoft.windowsappsdk/1.6.250108002/license.txt` — MICROSOFT SOFTWARE
  LICENSE TERMS, section 3(c)(ii): you may not *"modify or distribute the source code of any
  distributable code so that any part of it becomes subject to any license that requires that
  the distributable code, any other part of the software, or any of Microsoft's other
  intellectual property be disclosed or distributed in source code form, or that others have
  the right to modify it."* Section 3(b)(ii) additionally requires you to *"require distributors
  and external end users to agree to terms that protect it and Microsoft at least as much as
  this agreement."*

GPL-3.0 §10 forbids imposing further restrictions on downstream recipients, and §5/§6 require
the complete corresponding source of the whole combined work under GPL terms. §3(b)(ii) is a
further restriction that §7's closed list of permitted additional terms does not cover, and
§3(c)(ii) directly forbids the effect GPL-3.0 is designed to have. The Windows App SDK is also
not a GPL-3.0 §1 "System Library": it is not part of the normal packaging of Windows, which is
exactly why `WindowsAppSDKSelfContained` exists.

Three ways out, in order of preference:

1. **Add a GPL-3.0 §7 additional permission** covering linking with, and distributing the
   unmodified redistributables of, the Microsoft Windows App SDK. Scope it to that package
   alone: WebView2 (BSD-3-Clause) and every other .NET dependency here are already
   GPL-compatible, so a wider exception would give away more than the problem requires.
   This is the copyright holder's decision to make and cannot be inferred; it requires an
   explicit written exception. `LICENSE` stays the unmodified GPL text and the exception is
   stated separately.
2. **Stop redistributing the SDK**: set `WindowsAppSDKSelfContained` to `false` and require the
   user to install the Windows App Runtime. This removes the redistribution half of the
   problem; the linking half still needs option 1.
3. **Keep the tray out of the GPL publication.** The runtime, the client and the worker have no
   Windows App SDK dependency at all, so `voice-core-runtime.exe`, `voice-core.exe` and
   `worker/irodori/worker.py` are publishable under GPL-3.0 today.

`~/.nuget/packages/microsoft.windowsappsdk/1.6.250108002/NOTICE.txt` further shows the SDK
itself embeds third-party code (Newtonsoft.Json 13.0.1 MIT, among others) and states the
offer of source at `https://3rdpartysource.microsoft.com`. That NOTICE.txt is **not** currently
copied into the tray output or the package; Microsoft's §4(c) forbids removing supplier
notices, so it should travel with `bin/app/`.

---

## Bundled font — LXGW WenKai

| Component | Version | Licence | Why it is in the tree |
|---|---|---|---|
| LXGW WenKai Medium (`LXGWWenKai-Medium.ttf`) | shipped binary, 25,379,848 bytes | SIL Open Font License 1.1 | The subtitle overlay's typeface. `app/VoiceCoreTray/VoiceCoreTray.csproj:33` copies `assets\fonts\**` to the output, so the font and its licence travel together. |

`app/VoiceCoreTray/assets/fonts/OFL.txt` is the licence as shipped by the font author and
carries two copyright statements, both of which the OFL requires be reproduced with every copy:

```
Copyright 2021-2026 LXGW (https://github.com/lxgw/LxgwWenKai), with Reserved Font Name
'霞鹜', '霞鶩', '落霞孤鹜', '落霞孤鶩' and 'LXGW'.
Copyright 2020 The Klee Project Authors (https://github.com/fontworks-fonts/Klee)
```

OFL 1.1 §2 permits bundling and redistribution with any software *"provided that each copy
contains the above copyright notice and this license"*, included *"either as stand-alone text
files, human-readable headers or in the appropriate machine-readable metadata fields within
text or binary files"*. Shipping `OFL.txt` beside the `.ttf` satisfies this, and it is what the
csproj already does. §1 forbids selling the font by itself; §3 forbids using the Reserved Font
Names in a Modified Version — we ship the Original Version unmodified, so neither bites. OFL is
not a copyleft licence in the GPL sense and applies only to the font, so it does not interact
with the GPL terms on the code.

---

## Windows / .NET (portable package only)

Read from `*.nuspec` in `~/.nuget/packages/`. The first four are the `PackageReference` list in
`app/VoiceCoreTray/VoiceCoreTray.csproj:22-26`; the rest arrive transitively and land in the
tray output.

| Component | Version | Licence | Why it is in the tree |
|---|---|---|---|
| Microsoft.WindowsAppSDK | 1.6.250108002 | **Microsoft Software License Terms** (proprietary; `license.txt` in the package) — see BLOCKER above | WinUI 3 itself: the tray window, the acrylic backdrop, `AppWindow`, DWriteCore |
| H.NotifyIcon.WinUI | 2.3.0 | MIT | The tray icon and its context menu |
| System.Windows.Extensions | 8.0.0 | MIT | `System.Media.SoundPlayer`, which plays the WAV |
| WinUIEx | 2.5.1 | MIT | Window helpers on top of WinUI 3 |
| H.NotifyIcon | 2.3.0 | MIT | Transitive: the framework-agnostic half of H.NotifyIcon.WinUI |
| System.Collections.Immutable | 9.0.1 | MIT | Transitive via H.NotifyIcon.WinUI |
| System.Reflection.Metadata | 9.0.1 | MIT | Transitive via H.NotifyIcon.WinUI |
| Microsoft.Web.WebView2 | 1.0.2651.64 | **BSD-3-Clause** — the package's own `LICENSE.txt`, "Copyright (C) Microsoft Corporation", three-clause redistribution/endorsement text. Not the proprietary App SDK terms | Transitive via Microsoft.WindowsAppSDK. `Microsoft.Web.WebView2.Core.dll` plus `WebView2Loader.dll` for win-x64/x86/arm64 ship in the tray output even though the tray hosts no web content. Binary redistribution requires reproducing the copyright notice, which this file does |
| Microsoft.Windows.SDK.BuildTools | 10.0.22621.756 | Windows SDK licence, `licenseUrl: https://aka.ms/WinSDKLicenseURL` — **UNVERIFIED**: the package declares only a URL, no licence file on disk | Build-time tooling for the WinRT projections. Build-time only; verify before assuming nothing of it is redistributed |
| .NET 8 runtime (`Microsoft.NETCore.App`) | 8.0.0 requested | MIT (.NET runtime) | **Not redistributed.** `VoiceCoreTray.runtimeconfig.json` declares `framework` (not `includedFrameworks`) and the build output contains no `coreclr.dll`, `hostfxr.dll` or `System.Private.CoreLib.dll`, so the user's installed .NET 8 desktop runtime is used |

---

## Rust (source publication and portable package)

Every crate in `Cargo.lock` that is actually compiled into `voice-core-runtime.exe` /
`voice-core.exe` on Windows. Licences are the `license` field of each crate's own packaged
`Cargo.toml`, verbatim — including the inconsistent spellings upstream uses. Direct
dependencies are declared in `Cargo.toml:19-48`; everything else is transitive.

Ten further entries exist in `Cargo.lock` but are target-gated to macOS or `wasm32` and are
not present in the local registry, therefore not built and not redistributed:
core-foundation 0.9.4, js-sys 0.3.104, system-configuration 0.7.0, system-configuration-sys
0.6.0, wasm-bindgen 0.2.127, wasm-bindgen-futures 0.4.77, wasm-bindgen-macro 0.2.127,
wasm-bindgen-macro-support 0.2.127, wasm-bindgen-shared 0.2.127, web-sys 0.3.104.

Every licence below is MIT, Apache-2.0, BSD, ISC, Unicode-3.0, Unlicense, BSL-1.0 or a dual
offer among those. All are GPL-3.0-compatible; all require notice retention, which is what
this file provides. There is no copyleft and no proprietary term in the Rust tree.

| Component | Version | Licence |
|---|---|---|
| anstream | 1.0.0 | MIT OR Apache-2.0 |
| anstyle | 1.0.14 | MIT OR Apache-2.0 |
| anstyle-parse | 1.0.0 | MIT OR Apache-2.0 |
| anstyle-query | 1.1.5 | MIT OR Apache-2.0 |
| anstyle-wincon | 3.0.11 | MIT OR Apache-2.0 |
| anyhow | 1.0.104 | MIT OR Apache-2.0 |
| async-trait | 0.1.92 | MIT OR Apache-2.0 |
| atomic-waker | 1.1.2 | Apache-2.0 OR MIT |
| axum | 0.7.9 | MIT |
| axum-core | 0.4.5 | MIT |
| base64 | 0.22.1 | MIT OR Apache-2.0 |
| bitflags | 2.13.1 | MIT OR Apache-2.0 |
| bumpalo | 3.20.3 | MIT OR Apache-2.0 |
| bytes | 1.12.1 | MIT |
| cc | 1.4.4 | MIT OR Apache-2.0 |
| cfg-if | 1.0.4 | MIT OR Apache-2.0 |
| clap | 4.6.6 | MIT OR Apache-2.0 |
| clap_builder | 4.6.6 | MIT OR Apache-2.0 |
| clap_derive | 4.6.4 | MIT OR Apache-2.0 |
| clap_lex | 1.1.0 | MIT OR Apache-2.0 |
| colorchoice | 1.0.5 | MIT OR Apache-2.0 |
| core-foundation | 0.10.1 | MIT OR Apache-2.0 |
| core-foundation-sys | 0.8.7 | MIT OR Apache-2.0 |
| displaydoc | 0.2.7 | MIT OR Apache-2.0 |
| encoding_rs | 0.8.35 | (Apache-2.0 OR MIT) AND BSD-3-Clause |
| equivalent | 1.0.2 | Apache-2.0 OR MIT |
| errno | 0.3.14 | MIT OR Apache-2.0 |
| fastrand | 2.5.0 | Apache-2.0 OR MIT |
| find-msvc-tools | 0.1.11 | MIT OR Apache-2.0 |
| fnv | 1.0.7 | Apache-2.0 / MIT |
| foreign-types | 0.3.2 | MIT/Apache-2.0 |
| foreign-types-shared | 0.1.1 | MIT/Apache-2.0 |
| form_urlencoded | 1.2.2 | MIT OR Apache-2.0 |
| futures-channel | 0.3.34 | MIT OR Apache-2.0 |
| futures-core | 0.3.34 | MIT OR Apache-2.0 |
| futures-sink | 0.3.34 | MIT OR Apache-2.0 |
| futures-task | 0.3.34 | MIT OR Apache-2.0 |
| futures-util | 0.3.34 | MIT OR Apache-2.0 |
| getrandom | 0.2.17 | MIT OR Apache-2.0 |
| getrandom | 0.4.3 | MIT OR Apache-2.0 |
| h2 | 0.4.19 | MIT |
| hashbrown | 0.17.1 | MIT OR Apache-2.0 |
| heck | 0.5.0 | MIT OR Apache-2.0 |
| http | 1.5.0 | MIT OR Apache-2.0 |
| http-body | 1.1.0 | MIT |
| http-body-util | 0.1.5 | MIT |
| httparse | 1.10.1 | MIT OR Apache-2.0 |
| httpdate | 1.0.3 | MIT OR Apache-2.0 |
| hyper | 1.11.1 | MIT |
| hyper-rustls | 0.27.9 | Apache-2.0 OR ISC OR MIT |
| hyper-tls | 0.6.0 | MIT/Apache-2.0 |
| hyper-util | 0.1.20 | MIT |
| icu_collections | 2.3.0 | Unicode-3.0 |
| icu_locale_core | 2.3.0 | Unicode-3.0 |
| icu_normalizer | 2.3.0 | Unicode-3.0 |
| icu_normalizer_data | 2.3.0 | Unicode-3.0 |
| icu_properties | 2.3.0 | Unicode-3.0 |
| icu_properties_data | 2.3.0 | Unicode-3.0 |
| icu_provider | 2.3.1 | Unicode-3.0 |
| idna | 1.1.0 | MIT OR Apache-2.0 |
| idna_adapter | 1.2.2 | Apache-2.0 OR MIT |
| indexmap | 2.14.1 | Apache-2.0 OR MIT |
| ipnet | 2.12.1 | MIT OR Apache-2.0 |
| is_terminal_polyfill | 1.70.2 | MIT OR Apache-2.0 |
| itoa | 1.0.18 | MIT OR Apache-2.0 |
| libc | 0.2.189 | MIT OR Apache-2.0 |
| linux-raw-sys | 0.12.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| litemap | 0.8.3 | Unicode-3.0 |
| log | 0.4.34 | MIT OR Apache-2.0 |
| matchit | 0.7.3 | MIT AND BSD-3-Clause |
| memchr | 2.8.3 | Unlicense OR MIT |
| mime | 0.3.17 | MIT OR Apache-2.0 |
| mio | 1.2.2 | MIT |
| native-tls | 0.2.18 | MIT OR Apache-2.0 |
| once_cell | 1.21.4 | MIT OR Apache-2.0 |
| once_cell_polyfill | 1.70.2 | MIT OR Apache-2.0 |
| openssl | 0.10.81 | Apache-2.0 |
| openssl-macros | 0.1.1 | MIT/Apache-2.0 |
| openssl-probe | 0.2.1 | MIT OR Apache-2.0 |
| openssl-sys | 0.9.117 | MIT |
| percent-encoding | 2.3.2 | MIT OR Apache-2.0 |
| pin-project-lite | 0.2.17 | Apache-2.0 OR MIT |
| pkg-config | 0.3.34 | MIT OR Apache-2.0 |
| potential_utf | 0.1.6 | Unicode-3.0 |
| proc-macro2 | 1.0.107 | MIT OR Apache-2.0 |
| quote | 1.0.47 | MIT OR Apache-2.0 |
| r-efi | 6.0.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later |
| reqwest | 0.12.28 | MIT OR Apache-2.0 |
| ring | 0.17.14 | Apache-2.0 AND ISC |
| rustix | 1.1.4 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| rustls | 0.23.43 | Apache-2.0 OR ISC OR MIT |
| rustls-pki-types | 1.15.1 | MIT OR Apache-2.0 |
| rustls-webpki | 0.103.15 | ISC |
| rustversion | 1.0.23 | MIT OR Apache-2.0 |
| ryu | 1.0.23 | Apache-2.0 OR BSL-1.0 |
| schannel | 0.1.29 | MIT |
| security-framework | 3.7.0 | MIT OR Apache-2.0 |
| security-framework-sys | 2.17.0 | MIT OR Apache-2.0 |
| serde | 1.0.229 | MIT OR Apache-2.0 |
| serde_core | 1.0.229 | MIT OR Apache-2.0 |
| serde_derive | 1.0.229 | MIT OR Apache-2.0 |
| serde_json | 1.0.151 | MIT OR Apache-2.0 |
| serde_path_to_error | 0.1.20 | MIT OR Apache-2.0 |
| serde_urlencoded | 0.7.1 | MIT/Apache-2.0 |
| shlex | 2.0.1 | MIT OR Apache-2.0 |
| signal-hook-registry | 1.4.8 | MIT OR Apache-2.0 |
| slab | 0.4.12 | MIT |
| smallvec | 1.16.0 | MIT OR Apache-2.0 |
| socket2 | 0.6.5 | MIT OR Apache-2.0 |
| stable_deref_trait | 1.2.1 | MIT OR Apache-2.0 |
| strsim | 0.11.1 | MIT |
| subtle | 2.6.1 | BSD-3-Clause |
| syn | 2.0.119 | MIT OR Apache-2.0 |
| syn | 3.0.4 | MIT OR Apache-2.0 |
| sync_wrapper | 1.0.2 | Apache-2.0 |
| synstructure | 0.13.2 | MIT |
| tempfile | 3.27.0 | MIT OR Apache-2.0 |
| thiserror | 2.0.20 | MIT OR Apache-2.0 |
| thiserror-impl | 2.0.20 | MIT OR Apache-2.0 |
| tinystr | 0.8.4 | Unicode-3.0 |
| tokio | 1.53.1 | MIT |
| tokio-macros | 2.7.2 | MIT |
| tokio-native-tls | 0.3.1 | MIT |
| tokio-rustls | 0.26.4 | MIT OR Apache-2.0 |
| tokio-stream | 0.1.19 | MIT |
| tokio-util | 0.7.19 | MIT |
| tower | 0.5.3 | MIT |
| tower-http | 0.6.11 | MIT |
| tower-layer | 0.3.3 | MIT |
| tower-service | 0.3.3 | MIT |
| tracing | 0.1.44 | MIT |
| tracing-core | 0.1.36 | MIT |
| try-lock | 0.2.5 | MIT |
| unicode-ident | 1.0.24 | (MIT OR Apache-2.0) AND Unicode-3.0 |
| untrusted | 0.9.0 | ISC |
| url | 2.5.8 | MIT OR Apache-2.0 |
| utf8_iter | 1.0.4 | Apache-2.0 OR MIT |
| utf8parse | 0.2.2 | Apache-2.0 OR MIT |
| uuid | 1.26.0 | Apache-2.0 OR MIT |
| vcpkg | 0.2.15 | MIT/Apache-2.0 |
| want | 0.3.1 | MIT |
| wasi | 0.11.1+wasi-snapshot-preview1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| windows-link | 0.2.1 | MIT OR Apache-2.0 |
| windows-registry | 0.6.1 | MIT OR Apache-2.0 |
| windows-result | 0.4.1 | MIT OR Apache-2.0 |
| windows-strings | 0.5.1 | MIT OR Apache-2.0 |
| windows-sys | 0.52.0 | MIT OR Apache-2.0 |
| windows-sys | 0.59.0 | MIT OR Apache-2.0 |
| windows-sys | 0.61.2 | MIT OR Apache-2.0 |
| windows-targets | 0.52.6 | MIT OR Apache-2.0 |
| windows_aarch64_gnullvm | 0.52.6 | MIT OR Apache-2.0 |
| windows_aarch64_msvc | 0.52.6 | MIT OR Apache-2.0 |
| windows_i686_gnu | 0.52.6 | MIT OR Apache-2.0 |
| windows_i686_gnullvm | 0.52.6 | MIT OR Apache-2.0 |
| windows_i686_msvc | 0.52.6 | MIT OR Apache-2.0 |
| windows_x86_64_gnu | 0.52.6 | MIT OR Apache-2.0 |
| windows_x86_64_gnullvm | 0.52.6 | MIT OR Apache-2.0 |
| windows_x86_64_msvc | 0.52.6 | MIT OR Apache-2.0 |
| writeable | 0.6.4 | Unicode-3.0 |
| yoke | 0.8.3 | Unicode-3.0 |
| yoke-derive | 0.8.2 | Unicode-3.0 |
| zerofrom | 0.1.8 | Unicode-3.0 |
| zerofrom-derive | 0.1.7 | Unicode-3.0 |
| zeroize | 1.9.0 | Apache-2.0 OR MIT |
| zerotrie | 0.2.5 | Unicode-3.0 |
| zerovec | 0.11.8 | Unicode-3.0 |
| zerovec-derive | 0.11.6 | Unicode-3.0 |
| zmij | 1.0.23 | MIT |

---

## Python (portable package with `-IncludeEngine` only)

`worker/irodori/worker.py` imports `fastapi`, `pydantic`, `torch`, `uvicorn` and
`irodori_tts.inference_runtime` (worker.py:54-62, :197). It adds no dependency of its own: it
runs inside the engine's virtualenv, which the default package does **not** contain.
`scripts/package.ps1:176` copies that virtualenv into `runtime/python/` only when
`-IncludeEngine` is passed, and `:187` copies its base interpreter into `runtime/python-base/`.

The engine itself:

| Component | Version | Licence | Why it is in the tree |
|---|---|---|---|
| Irodori-TTS (upstream engine source) | v4 codebase for v4.1-Small | **MIT** — `voice-core/tts/irodori-tts/webui/Irodori-TTS/LICENSE`, "Copyright (c) 2026 Aratako"; `pyproject.toml:6` `license = "MIT"` | The synthesis implementation. `package.ps1:198` copies `webui/` to `runtime/engine/webui/`. MIT permits this redistribution with the copyright and permission notice retained, and MIT is GPL-3.0-compatible |
| DACVAE (vendored source tree, `webui/dacvae`) | 1.0.0 | **Apache-2.0** — the tree's own `LICENSE`; `setup.py` header "Copyright (c) Meta Platforms, Inc. and affiliates" | The continuous-latent audio codec the checkpoints generate into. `worker.py:51` puts it on `sys.path`. Apache-2.0 is GPL-3.0-compatible (one-way, into GPL-3.0) |
| CPython (base interpreter) | the venv's `pyvenv.cfg` `home` | PSF-2.0 | Runs the worker. Copied by `package.ps1:187` because a Windows venv records an absolute `home` |

Model weights, from the Hugging Face cache `package.ps1:205` copies into `models/huggingface/`:

| Component | Licence | Evidence | Why it is in the tree |
|---|---|---|---|
| `Aratako/Irodori-TTS-v4.1-Small` (`model.safetensors`, 3.06 GB) | MIT | model card front-matter `license: mit`, and the HF API `cardData.license` for the repo | The TTS checkpoint |
| `Aratako/Semantic-DACVAE-Japanese-32dim` (`weights.pth`, 429 MB) | MIT | HF API `cardData.license` = `mit`; the cached snapshot holds only `weights.pth`, no local card | 48 kHz codec for waveform reconstruction |
| `sbintuitions/modernbert-ja-310m` (`model.safetensors`, 1.26 GB) | MIT | model card front-matter `license: mit`, plus a `LICENSE` file in the snapshot | The shared text/caption encoder backbone |

The virtualenv's 148 installed distributions, from each `*.dist-info/METADATA`. Where METADATA
carried a copyright block or nothing usable, the wheel's own shipped `LICENSE` file was read
instead and is cited inline. Nothing here is GPL-incompatible: the strongest terms present are
`MPL-2.0` (certifi, tqdm, orjson — file-level copyleft, explicitly compatible with GPL-3.0
via MPL §3.3) and `LGPL-2.1-or-later` (soxr, upgradeable to LGPL-3 and thence GPL-3). The
engine also runs as a **separate process** behind a documented loopback HTTP protocol
(`docs/api.md:192-206`), not as a library linked into the runtime.

| Component | Version | Licence |
|---|---|---|
| absl-py | 2.5.0 | Apache-2.0 |
| accelerate | 1.14.0 | Apache |
| aiohappyeyeballs | 2.7.1 | PSF-2.0 |
| aiohttp | 3.14.3 | Apache-2.0 AND MIT |
| aiosignal | 1.4.0 | Apache 2.0 |
| annotated-doc | 0.0.5 | MIT |
| annotated-types | 0.8.0 | MIT |
| anyio | 4.14.2 | MIT |
| argbind | 0.3.9 | OSI Approved :: MIT License |
| asttokens | 3.0.2 | Apache 2.0 |
| attrs | 26.1.0 | MIT |
| brotli | 1.2.0 | MIT |
| certifi | 2026.7.22 | MPL-2.0 |
| cffi | 2.1.1 | MIT-0 |
| charset-normalizer | 3.5.1 | MIT |
| click | 8.5.0 | BSD-3-Clause |
| colorama | 0.4.6 | OSI Approved :: BSD License |
| contourpy | 1.3.3 | BSD 3-Clause License |
| cycler | 0.12.1 | BSD-3-Clause (per shipped `LICENSE`) |
| datasets | 5.0.1 | Apache 2.0 |
| decorator | 5.3.1 | BSD-2-Clause |
| descript-audiotools | 0.7.2 | MIT |
| dill | 0.4.1 | BSD-3-Clause |
| docstring_parser | 0.18.0 | MIT |
| einops | 0.8.2 | MIT |
| et_xmlfile | 2.0.0 | MIT |
| executing | 2.2.1 | MIT |
| fastapi | 0.141.1 | MIT |
| ffmpy | 1.0.0 | MIT |
| filelock | 3.32.3 | MIT |
| fire | 0.7.1 | Apache-2.0 |
| flatten-dict | 0.5.0 | MIT |
| fonttools | 4.63.0 | MIT |
| frozenlist | 1.8.0 | Apache-2.0 |
| fsspec | 2026.6.0 | BSD-3-Clause |
| gradio | 6.26.0 | Apache-2.0 |
| gradio_client | 2.6.1 | Apache-2.0 |
| groovy | 0.1.2 | OSI Approved :: MIT License |
| grpcio | 1.83.0 | Apache-2.0 |
| h11 | 0.16.0 | MIT |
| hf-gradio | 0.4.1 | MIT |
| hf-xet | 1.6.0 | Apache-2.0 |
| httpcore | 1.0.9 | BSD-3-Clause |
| httpx | 0.28.1 | BSD-3-Clause |
| huggingface_hub | 1.29.0 | Apache-2.0 |
| idna | 3.19 | BSD-3-Clause |
| importlib_resources | 7.1.0 | Apache-2.0 |
| ipython | 9.16.1 | BSD-3-Clause |
| ipython_pygments_lexers | 1.1.1 | OSI Approved :: BSD License |
| jedi | 0.20.0 | MIT |
| Jinja2 | 3.1.6 | OSI Approved :: BSD License |
| joblib | 1.5.3 | BSD-3-Clause |
| julius | 0.2.8 | MIT License |
| kiwisolver | 1.5.0 | BSD-3-Clause, "The Kiwi licensing terms" (per shipped `licenses/LICENSE`) |
| lazy-loader | 0.5 | BSD-3-Clause |
| librosa | 1.0.0 | ISC |
| llvmlite | 0.49.0 | BSD-2-Clause AND Apache-2.0 WITH LLVM-exception |
| Markdown | 3.10.3 | BSD-3-Clause |
| markdown-it-py | 4.2.0 | OSI Approved :: MIT License |
| markdown2 | 2.5.5 | MIT |
| MarkupSafe | 3.0.3 | BSD-3-Clause |
| matplotlib | 3.11.1 | matplotlib licence, PSF-style permissive (per shipped `LICENSE`) |
| matplotlib-inline | 0.2.2 | BSD-3-Clause |
| mdurl | 0.1.2 | OSI Approved :: MIT License |
| mpmath | 1.3.0 | BSD |
| msgpack | 1.2.2 | Apache-2.0 |
| multidict | 6.7.1 | Apache License 2.0 |
| multiprocess | 0.70.19 | BSD-3-Clause |
| narwhals | 2.25.0 | MIT |
| networkx | 3.6.1 | BSD-3-Clause |
| numba | 0.67.0 | BSD-2-Clause (per shipped `LICENSE`) |
| numpy | 2.5.2 | BSD-3-Clause AND 0BSD AND MIT AND Zlib AND CC0-1.0 |
| openpyxl | 3.1.5 | MIT |
| opentelemetry-api | 1.44.0 | Apache-2.0 |
| orjson | 3.12.0 | MPL-2.0 AND (Apache-2.0 OR MIT) |
| packaging | 26.3 | Apache-2.0 OR BSD-2-Clause |
| pandas | 3.0.5 | BSD 3-Clause License |
| parso | 0.8.7 | MIT |
| peft | 0.20.0 | Apache |
| pillow | 12.3.0 | MIT-CMU |
| platformdirs | 4.11.5 | MIT |
| pooch | 1.9.0 | BSD-3-Clause |
| prompt_toolkit | 3.0.53 | OSI Approved :: BSD License |
| propcache | 0.5.2 | Apache-2.0 |
| protobuf | 3.19.6 | 3-Clause BSD License |
| psutil | 7.2.2 | BSD-3-Clause |
| pure_eval | 0.2.3 | MIT |
| pyarrow | 25.0.1 | Apache-2.0 |
| pycparser | 3.0 | BSD-3-Clause |
| pydantic | 2.13.4 | MIT |
| pydantic_core | 2.46.4 | MIT |
| pydub | 0.25.1 | MIT |
| Pygments | 2.21.0 | BSD-2-Clause |
| pyloudnorm | 0.2.0 | MIT |
| pyparsing | 3.3.2 | MIT |
| pystoi | 0.4.1 | MIT |
| python-dateutil | 2.9.0.post0 | Apache-2.0 AND BSD-3-Clause (per shipped `LICENSE`) |
| python-multipart | 0.0.32 | Apache-2.0 |
| pytz | 2026.3.post1 | MIT |
| PyYAML | 6.0.3 | MIT |
| randomname | 0.2.1 | MIT License |
| regex | 2026.7.19 | Apache-2.0 AND CNRI-Python |
| requests | 2.34.2 | Apache-2.0 |
| Resemblyzer | 0.1.4 | Apache-2.0 (per shipped `Resemblyzer-0.1.4.dist-info/LICENSE`; METADATA says UNKNOWN) |
| rich | 15.0.0 | MIT |
| safehttpx | 0.1.7 | OSI Approved :: MIT License |
| safetensors | 0.8.0 | OSI Approved :: Apache Software License |
| scikit-learn | 1.9.0 | BSD-3-Clause |
| scipy | 1.18.1 | BSD-3-Clause (per shipped `LICENSE.txt`) |
| semantic-version | 2.10.0 | BSD |
| sentencepiece | 0.2.2 | Apache-2.0 |
| sentry-sdk | 2.68.1 | MIT |
| setuptools | 78.1.0 | OSI Approved :: MIT License |
| shellingham | 1.5.4 | ISC License |
| six | 1.17.0 | MIT |
| soundfile | 0.14.0 | BSD 3-Clause License |
| soxr | 1.1.0 | LGPL-2.1-or-later |
| stack-data | 0.6.3 | MIT |
| starlette | 1.6.0 | BSD-3-Clause |
| sympy | 1.14.0 | BSD |
| tensorboard | 2.20.0 | Apache 2.0 |
| tensorboard-data-server | 0.7.2 | Apache 2.0 |
| termcolor | 3.3.0 | MIT |
| threadpoolctl | 3.6.0 | BSD-3-Clause |
| tokenizers | 0.23.1 | OSI Approved :: Apache Software License |
| tomlkit | 0.14.0 | MIT |
| torch | 2.11.0+cu128 | BSD-3-Clause |
| torch-stoi | 0.2.3 | MIT |
| torchaudio | 2.11.0+cu128 | BSD-2-Clause (per shipped `LICENSE`) |
| torchcodec | 0.10.0 | BSD 3-Clause License |
| torchdata | 0.11.0 | BSD-3-Clause (per shipped `LICENSE`) |
| tqdm | 4.70.0 | MPL-2.0 AND MIT |
| traitlets | 5.16.1 | BSD 3-Clause License |
| transformers | 5.16.1 | Apache 2.0 License |
| typer | 0.27.1 | MIT |
| typing | 3.10.0.0 | PSF |
| typing-inspection | 0.4.4 | MIT |
| typing_extensions | 4.16.0 | PSF-2.0 |
| tzdata | 2026.3 | Apache-2.0 |
| urllib3 | 2.7.0 | MIT |
| uvicorn | 0.52.4 | BSD-3-Clause |
| wandb | 0.29.0 | MIT License |
| wcwidth | 0.8.2 | MIT |
| webrtcvad | 2.0.10 | MIT |
| webrtcvad-wheels | 2.0.14 | MIT |
| Werkzeug | 3.1.8 | BSD-3-Clause |
| xxhash | 4.0.1 | BSD-2-Clause |
| yarl | 1.24.5 | Apache-2.0 |

---

## Not third-party

`app/VoiceCoreTray/assets/icon.ico` is an original abstract waveform glyph on a gradient
squircle (7 frames, 16-256 px, PNG-compressed), authored for this project. It is not derived
from any game or third-party asset and is covered by this project's own licence.

## Not redistributed

These are read at build or evaluation time and appear in no artefact: `cargo`, `rustc`,
`dotnet`, `robocopy`, and `voice-core/tts/irodori-tts/eval/similarity_eval.py`'s Resemblyzer
dependency (Apache-2.0 per its shipped `LICENSE`; its METADATA declares UNKNOWN), which lives
in the engine venv only because the voice-quality evaluation used it.

Voice packs are **not** third-party components and are **not** publishable — see the audit in
`README.md` and ADR-0007 in the v1 checkout. `scripts/package.ps1:60` defaults `$VoicePacks` to
a directory of Blue Archive-trained LoRA adapters, which must not enter a public release.
