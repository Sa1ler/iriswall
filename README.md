# iriswall

Лёгкий демон обоев для **Hyprland** и других Wayland-композиторов с `wlr-layer-shell`.
Написан на Rust, рисует через EGL/GLES3. Меняет обои с плавными шейдерными анимациями,
пока ничего не происходит, не потребляет ни CPU, ни GPU.

- **6 переходов**: круг из центра, волна, рябь, мозаика, пролёт с размытием, растекание. Выбираются случайно при каждой смене
- **Папка с обоями** и **текущие обои в конфиге**: ничего не теряется после перезагрузки
- **Управление командами**: `next`, `prev`, `random`, `set`, `effect` и другие, удобно вешать на бинды
- **Экономный**: в простое кадры не рисуются, анимация идёт строго в такт монитору
- **Мониторы**: несколько экранов, подключение и отключение на лету, дробный масштаб (fractional-scale)
- **Меню выбора** для Quickshell с превью, см. [Интеграция с Quickshell](#интеграция-с-quickshell)
- **Палитра обоев и hook**: после каждой смены пишется `palette.json` и запускается твоя команда с цветами обоев, см. [Палитра и hook](#палитра-и-hook)
- **История**: `iriswall back` возвращает к тому, что было до этого, даже при случайном порядке
- **EXIF-ориентация**: фото с телефона больше не лежат на боку
- **Память под контролем**: настройки malloc и возврат памяти в простое встроены в сам демон

## Содержание

1. [Требования](#требования)
2. [Установка](#установка)
3. [Подключение к Hyprland](#подключение-к-hyprland)
4. [Команды](#команды)
5. [Конфиг](#конфиг)
6. [Эффекты перехода](#эффекты-перехода)
7. [Палитра и hook](#палитра-и-hook)
8. [Интеграция с Quickshell](#интеграция-с-quickshell)
9. [Обновление и удаление](#обновление-и-удаление)
10. [Потребление ресурсов](#потребление-ресурсов)
11. [Как это устроено](#как-это-устроено)
12. [Решение проблем](#решение-проблем)
13. [Ограничения](#ограничения)
14. [Лицензия](#лицензия)

## Требования

- Wayland-композитор с протоколом [`wlr-layer-shell`](https://wayland.app/protocols/wlr-layer-shell-unstable-v1): Hyprland, Sway, river и подобные
- Желательно [`viewporter`](https://wayland.app/protocols/viewporter) и [`fractional-scale-v1`](https://wayland.app/protocols/fractional-scale-v1): они есть в Hyprland. Без них дробный масштаб не работает
- Драйвер Mesa (или другой) с поддержкой **OpenGL ES 3.0** через EGL
- Rust (stable) для сборки

## Установка

### Arch Linux

```bash
sudo pacman -S --needed rust base-devel pkgconf wayland mesa libglvnd git

git clone https://github.com/Sa1ler/iriswall.git
cd iriswall
cargo build --release
install -Dm755 target/release/iriswall ~/.local/bin/iriswall
```

Папка `~/.local/bin` должна быть в `PATH`. Проверка: `iriswall --help`.

### Другие дистрибутивы

Нужны: компилятор Rust, `pkg-config`, и dev-пакеты `libwayland`, `libwayland-egl`, `libEGL` (Mesa). Дальше те же команды сборки.

## Подключение к Hyprland

В `~/.config/hypr/hyprland.conf`:

```ini
# запуск демона
exec-once = iriswall

# бинды (подставь свои клавиши)
bind = SUPER, W, exec, iriswall next
bind = SUPER SHIFT, W, exec, iriswall prev
bind = SUPER CTRL, W, exec, iriswall random
```

Выключи другие программы, которые рисуют обои на фоновом слое: hyprpaper, swww, фон в Quickshell и подобные. Иначе они будут перекрывать друг друга.

Переменные `MALLOC_*` больше не нужны: демон сам настраивает malloc при старте (см. [Потребление ресурсов](#потребление-ресурсов)).

## Команды

| Команда | Что делает |
|---|---|
| `iriswall` или `iriswall daemon` | запускает демон |
| `iriswall next` / `prev` | следующие или предыдущие обои из папки (по имени файла) |
| `iriswall back` | вернуться к предыдущим показанным обоям (до 50 шагов истории) |
| `iriswall random` | случайные обои |
| `iriswall set <файл>` | поставить конкретную картинку |
| `iriswall effect <имя>` | выбрать эффект перехода и сохранить его в конфиг: `random`, `iris`, `sweep`, `ripple`, `mosaic`, `zoom`, `ink` |
| `iriswall current` | путь к текущим обоям |
| `iriswall palette` | палитра текущих обоев в JSON |
| `iriswall reload` | перечитать конфиг и папку |
| `iriswall status` | состояние демона и потребление памяти (`mem_pss`, `mem_anon`, `mem_file`) |
| `iriswall quit` | остановить демон |

Управление идёт через сокет `$XDG_RUNTIME_DIR/iriswall.sock`. Это позволяет вызывать команды из скриптов, биндов и других программ.

## Конфиг

Файл `~/.config/iriswall/config.toml` создаётся при первом запуске. После правки выполни `iriswall reload`.

| Ключ | По умолчанию | Описание |
|---|---|---|
| `dir` | `~/Pictures/Wallpapers` | папка с обоями (jpg, png, webp, bmp, gif; без вложенных папок) |
| `current` | `""` | текущие обои. Демон обновляет строку сам, комментарии в файле сохраняются. Можно изменить вручную |
| `mode` | `fill` | `fill`: заполнить экран с обрезкой, `fit`: вписать целиком |
| `transition_ms` | `1400` | длительность перехода в миллисекундах, `0`: без анимации |
| `easing` | `ease-in-out` | `ease-in-out`, `ease-out` или `linear` |
| `effect` | `random` | `random` или имя одного эффекта |
| `effects` | все шесть | из каких эффектов выбирать при `effect = "random"` |
| `softness` | `0.18` | мягкость края (для `iris` и `ripple`) |
| `zoom` | `0.05` | насколько новые обои «оседают» из увеличения, `0`: выключено |
| `interval` | `0` | автосмена обоев раз в N секунд, `0`: выключено |
| `random` | `false` | случайный порядок при автосмене и команде `next` |
| `background` | `#000000` | цвет полей в режиме `fit` и фон до загрузки картинки |
| `hook` | `""` | команда после каждой смены обоев, получает цвета в переменных окружения, см. [Палитра и hook](#палитра-и-hook) |
| `trim_idle` | `true` | через ~3 с после смены возвращать память системе и вытеснять анонимные страницы в swap/zram |

Пример:

```toml
dir = "~/Pictures/Wallpapers"
current = "/home/user/Pictures/Wallpapers/forest.jpg"
mode = "fill"
transition_ms = 1400
effect = "random"
effects = ["iris", "ripple", "ink"]
interval = 900
```

## Эффекты перехода

При `effect = "random"` каждая смена обоев выбирает случайный эффект из списка `effects`. Один и тот же эффект два раза подряд не повторяется. Точка старта, угол и зерно шума тоже случайны.

| Эффект | Что происходит |
|---|---|
| `iris` | круг из центра экрана с мягким краем, новые обои «оседают» |
| `sweep` | мягкая волна под случайным углом, обе картинки слегка едут (параллакс) |
| `ripple` | круг от случайной точки с кольцом-рябью, преломляющим картинку |
| `mosaic` | плитки проявляются волной от случайной точки, каждая оседает с зумом |
| `zoom` | кроссфейд с радиальным размытием, как пролёт сквозь картинку |
| `ink` | органичное растекание по шуму от случайной точки с тёмной кромкой |

Зафиксировать один эффект: `iriswall effect ripple`. Вернуть случайный выбор: `iriswall effect random`.

## Палитра и hook

После каждой смены обоев демон выделяет из картинки до 6 доминирующих цветов и пишет их в
`~/.cache/iriswall/palette.json` (атомарно, через временный файл):

```json
{
  "wallpaper": "/home/user/Pictures/Wallpapers/forest.jpg",
  "dark": true,
  "primary": "#4f9d6b",
  "background": "#0e1a14",
  "foreground": "#d7e8dc",
  "colors": ["#0e1a14", "#4f9d6b", "#2c4a38", "#a8c9b4"]
}
```

`primary` — самый «живой» цвет для акцента, `background` и `foreground` — самый тёмный и самый
светлый из доминирующих, `dark` — обои в целом тёмные. Текущую палитру можно получить командой
`iriswall palette`.

Если в конфиге задан `hook`, он запускается через `sh -c` сразу после смены и не блокирует демон.
Ему доступны переменные `IRISWALL_PATH`, `IRISWALL_PRIMARY`, `IRISWALL_BACKGROUND`,
`IRISWALL_FOREGROUND`, `IRISWALL_DARK` (`1` или `0`), `IRISWALL_COLORS` (через пробел) и
`IRISWALL_PALETTE` (путь к JSON).

Пример: красить рамку активного окна в Hyprland. Скрипт `~/.config/scripts/wall-theme.sh`:

```sh
#!/bin/sh
c="${IRISWALL_PRIMARY#\#}"
hyprctl keyword general:col.active_border "rgb($c)"
```

В `config.toml`:

```toml
hook = "~/.config/scripts/wall-theme.sh"
```

Quickshell может следить за файлом и перекрашиваться сам:

```qml
FileView {
    path: Quickshell.env("HOME") + "/.cache/iriswall/palette.json"
    watchChanges: true
    onFileChanged: reload()
    JsonAdapter {
        property string primary: "#ffffff"
        property string background: "#000000"
        property bool dark: true
    }
}
```

## Интеграция с Quickshell

В папке [`quickshell/`](quickshell) лежит меню выбора обоев в стиле выбора тем: карусель с превью, управление стрелками и колесом, Enter или клик по центральной карточке применяют обои.

Как это работает:

- меню спрашивает у демона `iriswall status`, откуда брать папку и какие обои сейчас стоят
- список файлов берёт `FolderListModel`, превью грузятся уменьшенными и выгружаются из памяти после закрытия
- выбор применяется командой `iriswall set <путь>`, поэтому конфиг пишет только демон

Подключение (пример для DynamicIsland):

1. Положи `WallpaperPicker.qml` в папку `widgets/` рядом с остальными виджетами
2. В `DynamicIsland.qml` добавь режим `modeWallpaper`, его размеры и экземпляр `WallpaperPicker`, а в `shell.qml` функции `toggleWallpaper` и `closeWallpaper` (готовые версии файлов лежат в `quickshell/`)
3. Вызов меню:

```bash
qs ipc call topbar toggleWallpaper
```

Бинд в Hyprland:

```ini
bind = SUPER ALT, W, exec, qs ipc call topbar toggleWallpaper
```

Если Quickshell запущен с именем конфига, добавь его: `qs -c имя ipc call topbar toggleWallpaper`. Путь к бинарнику задаётся свойством `iriswallBin` в начале `WallpaperPicker.qml` (по умолчанию `~/.local/bin/iriswall`).

Для любого другого интерфейса достаточно одной команды: `iriswall set /путь/к/картинке.jpg`.

## Обновление и удаление

Обновить из клона репозитория:

```bash
cd iriswall
git pull
cargo build --release
iriswall quit
install -Dm755 target/release/iriswall ~/.local/bin/iriswall
iriswall &
```

Всё это можно сложить в скрипт:

```bash
#!/bin/sh
set -e
git pull --ff-only
cargo build --release
iriswall quit 2>/dev/null || true
install -Dm755 target/release/iriswall ~/.local/bin/iriswall
setsid iriswall >/dev/null 2>&1 &
```

Удаление:

```bash
iriswall quit
rm ~/.local/bin/iriswall
rm -r ~/.config/iriswall      # конфиг, по желанию
```

## Потребление ресурсов

Память процесса с EGL состоит из двух частей, и управлять можно только одной:

- **`Pss_File`**: библиотеки Mesa и драйвера. Это общие страницы, их делят с Hyprland и другими
  программами на GL, и пока демон рисует через OpenGL, они нужны. Меньше этой части не будет.
- **`Pss_Anon`**: данные самого процесса (куча, текстуры, память драйвера). Вот её демон и сжимает.

Что делает демон:

- **malloc настраивается сам** (одна арена, `mmap` для всего от 128 КБ). Раньше для этого нужны были
  переменные `MALLOC_*`, без них после декодирования большой картинки куча раздувалась и не
  сжималась (замеры на 8K показывали около 134 МБ против 56 МБ с переменными)
- **декодирование в RGB8**: без промежуточной RGBA-копии, пик при загрузке 4K/8K ниже на треть
- **текстуры RGB8 без mip-уровней**, если картинка нигде не показывается с заметным уменьшением:
  минус четверть памяти текстуры. Mip-уровни строятся только когда монитор заметно меньше картинки
- **пиксели на CPU освобождаются сразу** после загрузки в текстуру
- **в простое** (`trim_idle`): через ~3 с после конца перехода куча возвращается системе, а анонимные
  страницы вытесняются в swap/zram, если он есть. Без swap эта часть ничего не даёт
- **Mesa**: отключается дисковый кеш шейдеров и поток glthread (если ты не задал их сам)

CPU и GPU в простое: 0, кадры не рисуются.

Замерить свой результат:

```bash
iriswall status | grep mem_
# или напрямую
grep -E '^(Rss|Pss|Pss_Anon|Pss_File)' /proc/$(pidof iriswall)/smaps_rollup
```

Смотри на `Pss`, а не на `Rss`: `Rss` завышен общими страницами библиотек.

## Как это устроено

```
src/main.rs     Wayland (layer-shell, viewporter, fractional-scale), EGL, цикл событий (calloop), сокет управления
src/render.rs   декодирование картинок и GLES3-рендер, фрагментный шейдер со всеми эффектами
src/config.rs   конфиг, список файлов, easing, список эффектов
src/palette.rs  палитра обоев, palette.json, запуск hook
src/mem.rs      настройка malloc, возврат памяти в простое, отчёт о потреблении
```

- на каждый монитор своя поверхность на слое `background`, общий EGL-контекст и общие текстуры
- картинки декодируются в отдельном потоке и заранее уменьшаются до размера самого большого монитора
- анимация идёт по `wl_surface.frame`, прогресс считается по времени. Если композитор перестал присылать кадры (экран закрыт окном), итоговые обои применятся сразу
- вне перехода процесс спит в цикле событий

## Решение проблем

| Симптом | Что делать |
|---|---|
| не собирается из-за `-lEGL` или `egl.pc` | `sudo pacman -S libglvnd mesa pkgconf` |
| при запуске нет `libwayland-egl` | `sudo pacman -S wayland` |
| чёрный экран | запусти `iriswall` в терминале и читай сообщения; проверь `echo $WAYLAND_DISPLAY` |
| «нет подходящей EGL-конфигурации» | нужна поддержка GLES3 в драйвере, проверь `eglinfo` |
| «ошибка шейдера» | пришли текст ошибки в Issues, укажи видеокарту |
| «демон уже запущен» | `iriswall quit`; устаревший сокет демон удалит сам |
| `iriswall: демон не запущен` | запусти `iriswall` или проверь `exec-once` |
| меню Quickshell не видит `iriswall` | задай полный путь в `iriswallBin` |
| рендерит процессор (`llvmpipe`) | поставь драйвер видеокарты, проверь `eglinfo \| grep -i renderer` |

## Ограничения

- обои берутся только из одной папки, без вложенных
- на всех мониторах показывается одна и та же картинка
- нет видео и анимированных обоев (gif показывается первым кадром)
- без `viewporter` и `fractional-scale` дробный масштаб не поддерживается

## Лицензия

[MIT](LICENSE)
