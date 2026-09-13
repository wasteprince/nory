<div align="center">

<img src="assets/io.nory.NORY.svg" width="64" alt="Логотип NORY">

# NORY

VPN-клиент для Linux и Windows 11. Два ядра, один интерфейс.

[Скачать](https://github.com/wasteprince/nory/releases/latest) · [Что нового](https://github.com/wasteprince/nory/releases) · [Telegram](https://t.me/linuxset)

</div>

![NORY 0.3.10 — чёрно-графитовая тема](https://github.com/wasteprince/nory/releases/download/v0.3.10/NORY-0.3.10-dark.png)

<details>
<summary>Светлая тема</summary>

![NORY 0.3.10 — светлая тема](https://github.com/wasteprince/nory/releases/download/v0.3.10/NORY-0.3.10-light.png)

</details>

*Обе темы с демонстрационными серверами. Выбор оформления сохраняется.*

## Возможности

- TUN через **Xray + sing-box** или **Mihomo**.
- Несколько подписок, HWID и описания серверов.
- Просмотр JSON отдельного сервера по нажатию на его название.
- Конфигурации Xray JSON и Mihomo JSON, преобразование поддерживаемых протоколов и балансировщиков.
- Обход VPN для приложений, доменов и GeoIP/GeoSite; встроенные базы [RoscomVPN](assets/geodata/README.md) для обоих ядер.
- Сохранение правил при переносе JSON между ядрами; неподдерживаемые условия приводят к понятной ошибке.
- Светлая и чёрно-графитовая темы с белыми акцентами и стеклянной панелью.
- ICMP-пинг по умолчанию, корректные счётчики трафика Linux через API ядер, трей и логи.
- Подписанные обновления из GitHub.

## Установка

| Система | Пакет 0.3.10 | Как установить |
| --- | --- | --- |
| Arch Linux x86_64 | [pkg.tar.zst](https://github.com/wasteprince/nory/releases/download/v0.3.10/nory-0.3.10-1-x86_64.pkg.tar.zst) | `sudo pacman -U ./nory-0.3.10-1-x86_64.pkg.tar.zst` |
| Ubuntu 24.04+ / Debian 13+ amd64 | [deb](https://github.com/wasteprince/nory/releases/download/v0.3.10/nory_0.3.10_amd64.deb) | `sudo apt install ./nory_0.3.10_amd64.deb` |
| Windows 11 x64 | [Установщик](https://github.com/wasteprince/nory/releases/download/v0.3.10/NORY-0.3.10-windows-x64-setup.exe) | Запустить скачанный `.exe` |

Ядра входят в комплект. Перед ручным обновлением закройте NORY через трей — подписки и настройки сохранятся.

Linux требует systemd и WebKitGTK 4.1; пакетный менеджер установит зависимости.
Debian 12, Ubuntu 22.04, Windows 10 и ARM64 этой сборкой не поддерживаются.
На Windows может появиться SmartScreen; при отсутствии WebView2 установщику нужен интернет.
[Подробнее о Windows, правах и ограничениях](https://github.com/wasteprince/nory/blob/main/WINDOWS.md).

## Для разработчиков и провайдеров

NORY запрашивает подписки с `User-Agent: NORY/<версия>` (например, `NORY/0.3.10`), без подмены на Happ. Описание отдельного сервера читается из `meta.serverDescription` в его JSON-конфигурации.

<details>
<summary>Remnawave → Правила ответов: настройка описаний</summary>

В Remnawave откройте пункт **«Правила ответов»**. Добавьте этот объект в массив `rules` сразу после `Browser Subscription`, до `Fallback Base64`. Остальные правила оставьте без изменений: Remnawave использует первое совпавшее правило.

```json
{
  "name": "NORY",
  "description": "JSON с описаниями серверов для NORY",
  "enabled": true,
  "operator": "AND",
  "conditions": [
    {
      "headerName": "user-agent",
      "operator": "REGEX",
      "value": "^NORY/",
      "caseSensitive": false
    }
  ],
  "responseType": "XRAY_JSON",
  "responseModifications": {
    "additionalExtendedClientsRegex": ["^NORY/"]
  }
}
```

- `XRAY_JSON` выбирает JSON-выдачу, а `additionalExtendedClientsRegex` разрешает передачу описаний NORY. Нужны оба параметра.
- Заполните описание в настройках каждого хоста. Поле `description` в правиле выше — только комментарий к правилу, не описание сервера.
- Сохраните правила, обновите подписку в NORY и нажмите название выбранного сервера либо значок `{}` рядом. В сохранённом JSON должно появиться `meta.serverDescription`; тогда описание отобразится в карточке.
- Если NORY пишет, что JSON сформирован из ссылки, провайдер всё ещё отдаёт обычные ссылки: проверьте порядок правил и получение запроса с User-Agent NORY.
- Поддержка `additionalExtendedClientsRegex` проверена в Remnawave 3.4.3. Если ваша панель не принимает параметр, проверьте её версию и поддержку этой настройки. Менять User-Agent или отключать HWID не требуется.

[Порядок обработки правил](https://docs.rw/learn-en/routing-rules/) · [Параметр передачи описаний в Remnawave 3.4.3](https://github.com/remnawave/backend/blob/3.4.3/libs/contract/models/response-rules/response-rule-modifications.schema.ts#L74).

Не публикуйте JSON с рабочими паролями, ключами, ссылками подписок или HWID.

</details>

## Обратная связь

[Сообщить об ошибке](https://github.com/wasteprince/nory/issues) — укажите систему, версию и выбранное ядро. Не публикуйте ссылки подписок, пароли и HWID.

**TG канал разработчика:** [@linuxset](https://t.me/linuxset)

## Исходники

[Сборка и структура проекта](desktop/README.md). В репозитории — код клиента, ресурсы и файлы упаковки. Готовые установщики находятся в [Releases](https://github.com/wasteprince/nory/releases). [Архив исходников 0.3.10](https://github.com/wasteprince/nory/releases/download/v0.3.10/nory-0.3.10-source.tar.gz).

[Изменения в 0.3.10](https://github.com/wasteprince/nory/releases/tag/v0.3.10) включены в пакеты для всех трёх систем.

---

Rust · Tauri · Vue · TypeScript · Tailwind CSS

Ядра: [Xray](https://github.com/XTLS/Xray-core), [sing-box](https://github.com/SagerNet/sing-box), [Mihomo](https://github.com/MetaCubeX/mihomo).
