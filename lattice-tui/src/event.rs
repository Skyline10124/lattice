use crossterm::event::{self, Event as CEvent, KeyEvent, MouseEvent};
use std::time::Duration;
use tokio::sync::mpsc;

pub enum Event {
    Tick,
    Key(KeyEvent),
    Mouse(MouseEvent),
    /// LLM streaming token delivered from background task.
    StreamToken {
        content: String,
        reasoning: Option<String>,
        done: bool,
        error: Option<String>,
    },
    /// Resolved model + provider info from the background task.
    ModelInfo {
        model: String,
        provider: String,
    },
}

pub struct EventHandler {
    rx: mpsc::UnboundedReceiver<Event>,
    _tx: mpsc::UnboundedSender<Event>,
}

impl EventHandler {
    pub fn new(tick_rate: u64) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let tx_clone = tx.clone();
        let tick = Duration::from_millis(tick_rate);

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tick);
            loop {
                interval.tick().await;
                if tx_clone.send(Event::Tick).is_err() {
                    break;
                }
            }
        });

        let tx_clone = tx.clone();
        tokio::spawn(async move {
            loop {
                if let Ok(ready) = event::poll(Duration::from_millis(100)) {
                    if ready {
                        if let Ok(ev) = event::read() {
                            let mapped = match ev {
                                CEvent::Key(k) => Event::Key(k),
                                CEvent::Mouse(m) => Event::Mouse(m),
                                CEvent::Resize(_, _) => Event::Tick,
                                _ => continue,
                            };
                            if tx_clone.send(mapped).is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        });

        EventHandler { rx, _tx: tx }
    }

    pub async fn next(&mut self) -> Option<Event> {
        self.rx.recv().await
    }

    pub fn sender(&self) -> mpsc::UnboundedSender<Event> {
        self._tx.clone()
    }
}
