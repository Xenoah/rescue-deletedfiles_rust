"""Instrument only the diagnostic runner's GUI source; never used for release builds."""
from pathlib import Path

path = Path('crates/rescue-gui/src/main.rs')
source = path.read_text(encoding='utf-8')
before = '''        self.ime
            .normalize(ctx.memory(|memory| memory.focused()), &mut input.events);'''
after = '''        if !input.events.is_empty() {
            eprintln!("IME_RAW time={:?} focus={:?} value={:?} cursor={:?} events={:?}", input.time,
                ctx.memory(|m| m.focused()), self.filter,
                egui::TextEdit::load_state(ctx, egui::Id::new("result-search")).and_then(|s| s.cursor.char_range()), input.events);
        }
        self.ime
            .normalize(ctx.memory(|memory| memory.focused()), &mut input.events);
        if !input.events.is_empty() {
            eprintln!("IME_NORMAL time={:?} events={:?}", input.time, input.events);
        }'''
assert source.count(before) == 1
source = source.replace(before, after)
before = '''        self.receive();
        self.render(ctx);
    }
}'''
after = '''        self.receive();
        self.render(ctx);
        if ctx.input(|i| !i.events.is_empty()) {
            eprintln!("IME_FRAME time={} focus={:?} value={:?} cursor={:?}", ctx.input(|i| i.time),
                ctx.memory(|m| m.focused()), self.filter,
                egui::TextEdit::load_state(ctx, egui::Id::new("result-search")).and_then(|s| s.cursor.char_range()));
        }
    }
}'''
assert source.count(before) == 1
path.write_text(source.replace(before, after), encoding='utf-8')
