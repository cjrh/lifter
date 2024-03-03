pub enum LifterEvent {
    Input(crossterm::event::KeyEvent),
    Tick,
    Resize,
    Hello(i32),
    Message(String),
}
