<div align="center">

<img src="assets/nory.png" width="72" height="72" alt="Логотип NORY">

# NORY

Нативный VPN-клиент для macOS, Windows и Linux.

**[Скачать](#скачать)** · [Установка](INSTALL.md) · [Изменения](CHANGELOG.md) · [Сообщить об ошибке](https://github.com/wasteprince/nory/issues)

<br>

<a href="previews/macos.png"><img src="previews/macos.png" width="420" alt="NORY для macOS: Liquid Glass, список серверов и плавающая навигация"></a>

<sub>macOS · Liquid Glass · демонстрационные данные</sub>

</div>

## Скачать

**Windows 10 — [0.4.8](https://github.com/wasteprince/nory/releases/tag/v0.4.8) · Windows 11 и macOS Intel — [0.4.6](https://github.com/wasteprince/nory/releases/tag/v0.4.6) · Linux — [0.4.7](https://github.com/wasteprince/nory/releases/tag/v0.4.7) · macOS Apple Silicon — [0.4.1](https://github.com/wasteprince/nory/releases/tag/v0.4.1).**

| Система | Совместимость | Установщик |
| :--- | :--- | :--- |
| **macOS** | macOS 26+ · Apple Silicon, включая MacBook Neo | [Скачать PKG](https://github.com/wasteprince/nory/releases/download/v0.4.1/NORY-0.4.1-macos-arm64.pkg) |
| **macOS Intel** | macOS 26+ · x86_64 | [Скачать PKG](https://github.com/wasteprince/nory/releases/download/v0.4.6/NORY-0.4.6-macos-x86_64.pkg) |
| **Windows 11** | Windows 11 · x64 | [Скачать EXE](https://github.com/wasteprince/nory/releases/download/v0.4.6/NORY-0.4.6-windows-x64-setup.exe) |
| **Windows 10** | 21H2 / 22H2 / LTSC 2021 · x64 | [Скачать EXE](https://github.com/wasteprince/nory/releases/download/v0.4.8/NORY-0.4.8-windows10-x64-setup.exe) |
| **Ubuntu / Debian** | Ubuntu 24.04+ / Debian 13+ · amd64 | [Скачать DEB](https://github.com/wasteprince/nory/releases/download/v0.4.7/nory_0.4.7_amd64.deb) |
| **Arch Linux** | x86_64 · GTK4 / libadwaita | [Скачать пакет](https://github.com/wasteprince/nory/releases/download/v0.4.7/nory-0.4.7-1-x86_64.pkg.tar.zst) |

> **Windows 10 0.4.8:** отдельный пакет с системной рамкой, тёмным оформлением и собственным каналом обновлений. Среда выполнения и компоненты VPN входят в комплект.

> **Linux 0.4.7:** встроенные иконки, обновлённый интерфейс, исправление TUN и одноразовое разрешение VPN. Пакеты для Arch, Ubuntu и Debian.

> **Windows 0.4.6:** исправлена прокрутка колёсиком в режиме карточек; список сохраняет положение при обновлении серверов. Отключите VPN, завершите NORY и установите новый EXE поверх текущей версии. Подписки и настройки сохраняются.

<details>
<summary>Другие форматы и контрольные суммы</summary>

- macOS: [DMG](https://github.com/wasteprince/nory/releases/download/v0.4.1/NORY-0.4.1-macos-arm64.dmg) · [ZIP](https://github.com/wasteprince/nory/releases/download/v0.4.1/NORY-0.4.1-macos-arm64.zip).
- macOS Intel: [DMG](https://github.com/wasteprince/nory/releases/download/v0.4.6/NORY-0.4.6-macos-x86_64.dmg) · [ZIP](https://github.com/wasteprince/nory/releases/download/v0.4.6/NORY-0.4.6-macos-x86_64.zip).
- Windows 10: [ZIP](https://github.com/wasteprince/nory/releases/download/v0.4.8/NORY-0.4.8-windows10-x64.zip) для ручного развёртывания.
- Windows 11: [ZIP](https://github.com/wasteprince/nory/releases/download/v0.4.6/NORY-0.4.6-windows-x64.zip) для ручного развёртывания. Служба TUN устанавливается через EXE.
- SHA-256: [Windows 10 0.4.8](https://github.com/wasteprince/nory/releases/download/v0.4.8/SHA256SUMS.txt) · [Windows 11 / macOS Intel 0.4.6](https://github.com/wasteprince/nory/releases/download/v0.4.6/SHA256SUMS.txt) · [Linux 0.4.7](https://github.com/wasteprince/nory/releases/download/v0.4.7/SHA256SUMS.txt) · [macOS Apple Silicon 0.4.1](https://github.com/wasteprince/nory/releases/download/v0.4.1/SHA256SUMS.txt).

</details>

При переходе с 0.3.x нужна [ручная установка](INSTALL.md#windows). Требования к системе и особенности подписи пакетов — в [инструкции](INSTALL.md).

## Возможности

- **Подписки:** несколько источников, описания серверов, лимиты трафика и срок действия.
- **Серверы:** список или карточки, поиск, проверка задержки и круглые флаги.
- **Маршрутизация:** routing, DNS и outbounds из JSON сервера; обход VPN для приложений и адресов.
- **Интерфейс:** тёмное оформление, системные кнопки окна, плавающая навигация и журнал подключений.

Xray + sing-box TUN. Нативные интерфейсы: SwiftUI на macOS, WinUI 3 на Windows, GTK4 / libadwaita на Linux. Android и iOS пока не выпущены.

---

[Релизы](https://github.com/wasteprince/nory/releases) · [Установка](INSTALL.md) · [История изменений](CHANGELOG.md) · [Обратная связь](https://github.com/wasteprince/nory/issues)

<sub>Публичный репозиторий содержит установщики, документацию и превью. Исходники не публикуются. Лицензии компонентов входят в пакеты.</sub>
