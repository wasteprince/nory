# NORY 0.3.10

В этой версии сохраняются правила доменов и GeoIP/GeoSite при переносе JSON между
Xray и Mihomo. Правила, которые второе ядро не умеет выполнить, приводят к понятной
ошибке с предложением выбрать исходное ядро, а не исчезают при подключении.
Xray проверяет GeoIP до завершающего правила, Mihomo получает встроенные GeoData
для любого JSON. Базы RoscomVPN включены в установщики; стандартные категории,
которых нет в RoscomVPN, сохранены для совместимости. Источники и контрольные суммы:
`assets/geodata/README.md` и `scripts/tauri/geodata.py` в исходниках.

На Linux «Получено / Отправлено» теперь берутся из суммарных счётчиков ядра.
Счётчики интерфейса TUN не учитывали TCP, перенаправленный через nftables.
Xray предоставляет статистику через HTTP metrics на локальном API-порту;
Mihomo — через свой controller API. Временная ошибка опроса сохраняет последнее
значение, а новое подключение начинает счётчики с нуля.

Добавлены светлая и графитовая темы с переключателем и сохранением выбора.
Плавающая панель использует полупрозрачное стекло. Постоянных анимаций нет;
настройки системы для уменьшения прозрачности и движения учитываются.



Текущий клиент: Vue 3 + TypeScript + Tailwind CSS, оболочка Tauri 2 и общий Rust-бэкенд.

В 0.3.9 исправлен ложный статус подключения Xray на Linux и Windows: NORY ждёт
полного запуска sing-box, проверяет TUN перед подключением и отслеживает его
остановку. Краткое пересоздание TUN не вызывает ложного отключения, а другой
процесс NORY больше не может остановить активный Linux-сеанс. При обновлении
Debian также перезапускается системный helper.

Скрытое окно прекращает опросы интерфейса; неактивные страницы и неизменившиеся
карточки серверов не перерисовываются при обновлении счётчиков. Проверки TUN
для окна, трея и переподключения объединены. ICMP выбран способом пинга по
умолчанию для новых настроек; сохранённый выбор не меняется.

В 0.3.8 нажатие на название выбранного сервера открывает JSON только этого хоста.
Балансировщики и исходные конфигурации сохраняются целиком; для обычных ссылок
формируется JSON Xray. Просмотр не меняет подключение.

## Структура

- `src/` — подключение, подписки, конвертация конфигураций, обновления, обходы и системный helper.
- `desktop/src/` — интерфейс Vue.
- `desktop/src-tauri/` — окно, IPC, трей и интеграция с системой.
- `assets/` — логотип, карта и ресурсы. PNG-флаги встраиваются только в Windows.
- `scripts/tauri/` и `packaging/` — сборка и упаковка для Arch, Debian/Ubuntu и Windows 11 x64.

В корневом Rust-пакете также сохранён предыдущий GTK-интерфейс под опциональной возможностью `native-ui`. Tauri использует общий бэкенд без этой возможности; корневая команда `cargo build` сама по себе не собирает новый интерфейс.

## Запуск для разработки

Нужны актуальные Rust stable с поддержкой edition 2024, Node.js 22.12+ и npm.
На Linux установите средства компиляции, `pkg-config`, WebKitGTK 4.1, GTK 3, OpenSSL и AppIndicator. Для TUN нужны systemd, polkit, iproute2 и nftables.
На Windows 11 x64 нужны MSVC Build Tools, Windows SDK и WebView2.

Из корня репозитория:

```sh
cd desktop
npm ci
npm run tauri -- dev
```

Запуск одного `npm run dev` открывает только фронтенд: системные функции требуют оболочку Tauri.
Для TUN нужен установленный NORY helper: Linux использует `nory-helper.socket`, Windows — службу `NoryTunnel`. GUI не следует запускать от root.

## Сборка исполняемых файлов

```sh
cd desktop
npm ci
npm run build
cd ..
cargo build --release --locked --manifest-path desktop/src-tauri/Cargo.toml
cargo build --release --locked --no-default-features --bin nory-helper
```

Результат: `desktop/src-tauri/target/release/nory` и `target/release/nory-helper` (на Windows — с расширением `.exe`). Это ещё не установочный пакет.

## Упаковка

Linux-упаковка ожидает официальные ядра x86_64 и их лицензии в локальном каталоге, который не входит в Git:

```text
.bundled/arch/
  xray/      xray, LICENSE, geoip.dat, geosite.dat
  sing-box/  sing-box, LICENSE, libcronet.so
  mihomo/    mihomo, LICENSE
```

Получите их из официальных релизов Xray, sing-box и Mihomo, проверив опубликованные контрольные суммы. Сборки NORY 0.3.6 используют sing-box 1.13.18; его `libcronet.so` должен соответствовать версии ядра.
Установщики содержат эти бинарники; в исходники они не включены.

Для Arch подготовьте **новый** staging-каталог, затем используйте `makepkg`:

```sh
bash scripts/tauri/stage-linux.sh "$PWD/packaging/tauri/arch/stage" \
  "$PWD/desktop/src-tauri/target/release/nory" "$PWD/target/release/nory-helper"
cp packaging/nory.install packaging/tauri/arch/nory.install
cd packaging/tauri/arch
makepkg --nodeps --nocheck
```

Для полного набора пакетов на Linux используются:

```sh
bash scripts/tauri/ubuntu-build.sh "$PWD/.cache/ubuntu"
bash scripts/tauri/windows-build.sh "$PWD/.cache/windows"
bash scripts/tauri/package.sh "$PWD/releases/local-build" \
  "$PWD/.cache/ubuntu" "$PWD/.cache/windows"
```

Перед этим соберите фронтенд, Linux GUI и helper командами выше. Полная упаковка требует Arch `makepkg`, bubblewrap, разрешённые user namespaces с subordinate UID/GID, Python 3.11+, curl, bsdtar, objdump, LLVM и cargo-xwin. Каталог результата должен быть новым. Ubuntu-сборка выполняется в отдельном rootfs; Windows собирается для MSVC со статическим CRT. Windows-ядра и Wintun скачиваются упаковщиком с проверкой закреплённых SHA-256. При изменении WebView2 bootstrapper упаковщик остановится на несовпадении хеша: новую версию необходимо проверить отдельно.

Ключи подписи обновлений, пользовательские подписки, `.env`, тесты, логи, зависимости и готовые сборки не публикуются. Публичный ключ проверки обновлений в коде не является секретом.
