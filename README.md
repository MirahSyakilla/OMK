# Oh My Keymint

[![Telegram](https://img.shields.io/static/v1?label=Telegram&message=@meowcomfylair&color=0088cc)](https://t.me/meowcomfylair)

Fork of [qwq233/OhMyKeymint](https://github.com/qwq233/OhMyKeymint). A full Android Keystore 2.0 / KeyMint stand-in: scooped apps talk only to OMK, not the vendor TEE.

## What it does

OMK implements the AOSP Keystore2 AIDL surface in-process and injects into `keystore2`. Packages listed in `scoop` are routed to OMK. Everything else stays on hardware.

Compared with TrickyStore / TEESimulator, OMK owns the whole Keystore path for those apps (not only KeyMint generate/attest). Detectors that probe binder shape, grants, and operation timing hit the same service.

## Requirements

- Android 12–17
- KernelSU, APatch, or Magisk
- arm64-v8a

## Install

1. Flash the module zip and reboot.
2. Open the module WebUI (KernelSU / APatch) or edit the files under `/data/misc/keystore/omk/`.
3. Put apps in `scoop` (`injector.toml`) and install a keybox if you need hardware-looking attestation.

Active files:

- `/data/misc/keystore/omk/config.toml` — KeyMint identity, trust, crypto seeds
- `/data/misc/keystore/omk/injector.toml` — scoop, intercept switches
- `/data/misc/keystore/omk/keybox.xml` — attestation signing keys

See [Configuration Guide](docs/CONFIGURATION.md).

## Keybox

A valid box has at least one RSA or EC entry whose private key matches its chain. PKCS#1, SEC1, and PKCS#8 PEM are accepted. RKP-style EC-only boxes work; RSA attest requests are signed with that EC key.

If `keybox.xml` is missing, OMK writes the bundled AOSP software template. If the file is invalid, it is left on disk and OMK keeps the last valid box (or the bundled template) in memory so keymint still starts.

WebUI Keybox menu: Manage Keybox, AOSP (bundled), Self-Signed (local dummy), AlwaysStrong (Evoker fetch), Local file, Repo (KOWX712), Custom URL. Manage can rename, delete, and export a slot to `/storage/emulated/0/Download/OMK/`. Long-press an app to assign a slot. Play Services and Play Store show a PIF pill while Integrity Settings is enabled.

## Restart

WebUI: restart icon (between search and the overflow menu) → Daemon, Injector, or All. Confirm first.

Shell:

```sh
touch /data/adb/omk/restart.keymint
touch /data/adb/omk/restart.injector
touch /data/adb/omk/restart.all
```

Injector-only setting changes do not need a keymint restart. Trust fields other than the four patch levels do.

Restart Daemon/All re-delivers the last device-unlock to the new keymint process
from the injector's cache, so CredentialEncrypted and auth-bound keys keep working
without a reboot. If the device is locked when keymint restarts there is nothing
to replay until the next unlock.

## License

**YOU MUST AGREE TO BOTH OF THE LICENSE BEFORE USING THIS SOFTWARE.**

`AGPL-3.0-or-later`

```plaintext
OhMyKeymint - Custom keymint implementation for Android Keystore Spoofer
Copyright (C) 2025 James Clef <qwq233@qwq2333.top>

This program is free software: you can redistribute it and/or modify
it under the terms of the GNU Affero General Public License as
published by the Free Software Foundation, either version 3 of the
License, or (at your option) any later version.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
GNU Affero General Public License for more details.

You should have received a copy of the GNU Affero General Public License
along with this program.  If not, see <https://www.gnu.org/licenses/>.
```

`Oh My Keymint License`

```plaintext
1. 您不得将本软件、本软件的任意部分或将本软件作为依赖的软件用于任何商业用途。该
   商业用途包括但不限于以盈利为目的，将本软件、本软件的任意部分或将本软件作为依
   赖的软件与其他资源、物品或服务捆绑销售。

2. 您不得暗示或明示本软件与其他软件有任何从属关系。

3. 未经本软件作者书面允许，您不得超出合理使用范围或协议许可范围使用本软件的名称。

4. 除非您所在的司法管辖区的适用法律另行规定，您同意将纠纷或争议提交至中国大陆境
   内有管辖权的人民法院管辖。

5. 本协议与GNU Affero General Public License（以下简称AGPL）共同发挥效力，
   当本协议内容与AGPL冲突时，应当优先应用本协议内容，本协议仅覆盖本软件作者拥有
   完全著作权的部分，对于使用其他协议的软件代码不发挥效力。
```

## Credit

Upstream: [qwq233/OhMyKeymint](https://github.com/qwq233/OhMyKeymint) (James Clef).

Some code from [AOSP](https://source.android.com/) (`Apache-2.0`).
