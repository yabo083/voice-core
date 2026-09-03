# GPL-3.0 §7 additional permission — Microsoft Windows App SDK

Copyright (C) 2026 miyako

## Why this file exists

`VoiceCoreTray.exe` links against the Microsoft Windows App SDK (WinUI 3) and
ships its unmodified redistributable components. Those components are covered by
the *Microsoft Software License Terms — Microsoft Windows App SDK*, which
requires downstream terms that GPL-3.0 §10 forbids a licensee from accepting,
and forbids (§3(c)(ii)) exactly the source-availability effect the GPL exists to
produce. GPL-3.0 §1's System Library exemption does not rescue the combination:
the Windows App SDK is not part of the normal packaging of Windows, which is the
whole reason `WindowsAppSDKSelfContained` exists.

GPL-3.0 §7 anticipates this and permits the copyright holder to grant additional
permissions. Without one, the tray would not be distributable in binary form.
The runtime (`voice-core-runtime.exe`), the client (`voice-core.exe`) and the
Python worker have no Windows App SDK dependency and are unaffected either way.

## The grant

Additional permission under GNU GPL version 3 section 7:

If you modify this Program, or any covered work, by linking or combining it with
the Microsoft Windows App SDK (including the WinUI 3 libraries and the Windows
App Runtime), or with a modified version of those components, containing parts
covered by the terms of the Microsoft Software License Terms for the Microsoft
Windows App SDK, the licensors of this Program grant you the additional
permission to convey the resulting work and to distribute the unmodified
redistributable components of the Microsoft Windows App SDK that the resulting
work requires in order to run.

The Corresponding Source for a non-source form of such a combination shall
include the source code for the parts of the covered work used to produce it,
but need not include the source code for the Microsoft Windows App SDK, to the
extent that such source is unavailable under terms permitting its inclusion. In
that case this additional permission applies only to the covered work.

If you modify this Program, you may extend this permission to your version of
the Program, but you are not obliged to do so. If you do not wish to do so,
delete this statement from your version.

## What it does not do

- It grants nothing with respect to Microsoft's own terms. Anyone redistributing
  the Windows App SDK components remains bound by them, and their `license.txt`
  and `NOTICE.txt` must travel with the binaries (`scripts/package.ps1` copies
  both into `bin/app/`).
- It is scoped to the Windows App SDK alone. Every other component this project
  redistributes is GPL-3.0-compatible on its own terms; see
  `THIRD-PARTY-NOTICES.md`.
- It changes nothing about the source publication, which was never blocked: no
  Microsoft code is in this repository (`app/**/bin/` and `app/**/obj/` are
  ignored).
