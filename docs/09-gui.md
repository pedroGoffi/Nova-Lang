# GUI and the Emacs-Inspired Editor

The [`gui`](../examples/features/gui/src/main.nova) example uses `nova_gui`, an object-oriented layer over SDL2 and SDL2_ttf. Native handles remain inside the platform implementation.

## Setup and window ownership

```toml
nova_gui = { path = "../../../packages/nova_gui" }
```

```nova
import nova_gui;
use nova_gui::window::Window;

let window = Window::open("Nova GUI", 800, 600);
```

`Window` owns the SDL window, renderer, and event buffer. Close it after dependent widgets.

## Builder-based panels

```nova
let panel = Panel::builder(16, 16, 768, 568)
    .fill(28, 40, 60, 255)
    .border(70, 150, 210, 2)
    .padding(16)
    .shadow(true)
    .header(true, 30)
    .accent(70, 220, 255)
    .grid(true)
    .build();
```

Each method updates and returns the builder, making configuration readable without a long positional constructor.

## Fonts and event loop

```nova
sdl2::initialize_ttf();
let font = sdl2::Font::open(
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", 18
);
```

The font path must exist. SDL_ttf performs multiline UTF-8 rendering.

```nova
let mut running = true;
while running {
    let event = window.poll();
    if event == sdl2::EVENT_QUIT { running = false; }
    editor.handle_event(event, window.event_text(), window.event_key(), window.event_mod());
    window.clear(20, 24, 32, 255);
    window.draw_editor(&editor);
    window.present();
}
```

Without a persistent event loop, `main` returns and the window closes immediately. A short delay when no event is available avoids a busy loop.

## Real buffers and editor commands

The editor starts with no fake tabs. Opening a file creates a real buffer, and the loop synchronizes the tab bar and buffer list with the editor state.

| Shortcut | Action |
| --- | --- |
| `C-x C-f` | Open or create a file |
| `C-x C-s` | Save the current buffer |
| `C-x b` | Switch buffers |
| `C-s` | Incremental search |
| `C-SPC` | Set the mark |
| `C-w`, `M-w` | Kill or copy the region |
| `C-k`, `C-y` | Kill a line or yank |
| `M-f`, `M-b` | Move by word |
| `C-a`, `C-e` | Move to line start or end |
| `M-x` | Execute a named command |

Current `M-x` commands include `save-buffer`, `find-file`, `next-buffer`, `beginning-of-buffer`, `end-of-buffer`, `kill-region`, and `yank`.

Run it with `cargo run -- run examples/features/gui`. Shutdown order is: stop text input, close widgets/fonts, shut down SDL_ttf, then close the window.
