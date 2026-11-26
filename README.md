# Поиск дублей

Программа с окном ищет одинаковые файлы в выбранной папке и показывает, какие форматы там есть. Сама ничего не стирает: лишние копии можно убрать в корзину кнопкой «оставить».

Собирайте её на той системе, где она будет запускаться. Окно рисуется средствами этой системы, поэтому отдельная кросс-компиляция с macOS на Windows или Linux здесь не описана.

Нужен [Rust](https://rustup.rs) 1.81 или новее. Шрифты с кириллицей уже лежат в `assets/`.

## macOS

Один раз поставьте инструменты командной строки Xcode, если их ещё нет:

```bash
xcode-select --install
```

В каталоге проекта:

```bash
cargo build --release
./target/release/duplicate-searcher
```

## Linux

На Debian и Ubuntu:

```bash
sudo apt-get install build-essential pkg-config libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev libssl-dev
```

На Fedora:

```bash
sudo dnf install gcc pkgconf-pkg-config libxcb-devel libxkbcommon-devel openssl-devel
```

Диалог выбора папки на Linux идёт через xdg-desktop-portal. В GNOME и KDE он обычно уже установлен.

```bash
cargo build --release
./target/release/duplicate-searcher
```

## Windows 11

Поставьте Rust через rustup и выберите цепочку MSVC. Вместе с ней нужны «Средства сборки Visual Studio» с компонентом «Разработка классических приложений на C++»: компилятор, компоновщик и Windows SDK. Из SDK при сборке берётся `rc.exe`, им в программу встраивается манифест с поддержкой длинных путей.

В каталоге проекта, в обычной консоли:

```bat
cargo build --release
target\release\duplicate-searcher.exe
```

## Проверка

```bash
cargo test
```
