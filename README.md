<div align="center">

# NORY

Нативный VPN-клиент для macOS, Windows и Linux.

**Один интерфейс по смыслу. Системные элементы на каждой платформе.**

[Скачать NORY 0.4.0](https://github.com/wasteprince/nory/releases/tag/v0.4.0) · [Установка](INSTALL.md) · [Что нового](CHANGELOG.md)

<a href="previews/macos.png"><img src="previews/macos.png" width="440" alt="NORY для macOS — тёмный нативный интерфейс, серверы и плавающая навигация"></a>

macOS · SwiftUI и системный Liquid Glass

</div>

## Скачать

| Платформа | Требования | Загрузка |
| --- | --- | --- |
| macOS | macOS 26+, Apple Silicon, включая MacBook Neo | [PKG](https://github.com/wasteprince/nory/releases/download/v0.4.0/NORY-0.4.0-macos-arm64.pkg) · [DMG](https://github.com/wasteprince/nory/releases/download/v0.4.0/NORY-0.4.0-macos-arm64.dmg) · [ZIP](https://github.com/wasteprince/nory/releases/download/v0.4.0/NORY-0.4.0-macos-arm64.zip) |
| Windows | Windows 11, x64 | [Установщик](https://github.com/wasteprince/nory/releases/download/v0.4.0/NORY-0.4.0-windows-x64-setup.exe) · [ZIP](https://github.com/wasteprince/nory/releases/download/v0.4.0/NORY-0.4.0-windows-x64.zip) |
| Ubuntu / Debian | Ubuntu 24.04+ или Debian 13+, amd64 | [DEB](https://github.com/wasteprince/nory/releases/download/v0.4.0/nory_0.4.0_amd64.deb) |
| Arch Linux | x86_64, актуальные GTK4 и libadwaita | [Пакет Arch](https://github.com/wasteprince/nory/releases/download/v0.4.0/nory-0.4.0-1-x86_64.pkg.tar.zst) |

При переходе с 0.3.x установите новый пакет вручную, предварительно завершив NORY. Подписки и настройки хранятся отдельно от приложения. Для последующих обновлений Windows и Linux используется проверка подписи.

## Возможности

- Xray для подключений и sing-box для TUN. Ядро Mihomo полностью убрано из новых сборок.
- Несколько подписок, их описания и лимиты трафика; полные описания серверов.
- Вертикальный список или карточки, круглые флаги, поиск и ручная проверка задержки.
- Исходный Xray JSON каждого сервера используется при подключении вместе с его правилами роутинга, DNS и outbounds.
- Обход VPN для приложений и адресов, журнал подключений и настройки сети.
- Тёмное оформление без переключателя темы, системные оконные кнопки и плавающая нижняя навигация.
- macOS: SwiftUI / AppKit и Liquid Glass. Windows: WinUI 3 и Desktop Acrylic. Linux: GTK4 / libadwaita с матовыми поверхностями.

Прозрачность на macOS и Windows зависит от системных настроек эффектов и доступности. Приложения для Android и iOS пока не выпущены.

Превью macOS использует демонстрационные подписки; задержки и трафик приведены для примера. Нажмите на изображение, чтобы открыть PNG в полном разрешении.

## О репозитории

Здесь публикуются готовые сборки, описание приложения и превью. Исходники NORY в публичный репозиторий не включены. Лицензии используемых компонентов входят в установочные пакеты.
