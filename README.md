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

**Windows — [0.4.2](https://github.com/wasteprince/nory/releases/tag/v0.4.2) · macOS и Linux — [0.4.1](https://github.com/wasteprince/nory/releases/tag/v0.4.1).**

| Система | Совместимость | Установщик |
| :--- | :--- | :--- |
| **macOS** | macOS 26+ · Apple Silicon, включая MacBook Neo | [Скачать PKG](https://github.com/wasteprince/nory/releases/download/v0.4.1/NORY-0.4.1-macos-arm64.pkg) |
| **Windows** | Windows 11 · x64 | [Скачать EXE](https://github.com/wasteprince/nory/releases/download/v0.4.2/NORY-0.4.2-windows-x64-setup.exe) |
| **Ubuntu / Debian** | Ubuntu 24.04+ / Debian 13+ · amd64 | [Скачать DEB](https://github.com/wasteprince/nory/releases/download/v0.4.1/nory_0.4.1_amd64.deb) |
| **Arch Linux** | x86_64 · GTK4 / libadwaita | [Скачать пакет](https://github.com/wasteprince/nory/releases/download/v0.4.1/nory-0.4.1-1-x86_64.pkg.tar.zst) |

> **Windows 0.4.2:** исправлена ошибка «Ядро отклонило конфигурацию» при запуске VPN. Отключите VPN, завершите NORY и установите новый EXE поверх текущей версии. Подписки и настройки сохраняются.

<details>
<summary>Другие форматы и контрольные суммы</summary>

- macOS: [DMG](https://github.com/wasteprince/nory/releases/download/v0.4.1/NORY-0.4.1-macos-arm64.dmg) · [ZIP](https://github.com/wasteprince/nory/releases/download/v0.4.1/NORY-0.4.1-macos-arm64.zip).
- Windows: [ZIP](https://github.com/wasteprince/nory/releases/download/v0.4.2/NORY-0.4.2-windows-x64.zip) для ручного развёртывания. Служба TUN устанавливается через EXE.
- SHA-256: [Windows 0.4.2](https://github.com/wasteprince/nory/releases/download/v0.4.2/SHA256SUMS.txt) · [macOS / Linux 0.4.1](https://github.com/wasteprince/nory/releases/download/v0.4.1/SHA256SUMS.txt).

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
