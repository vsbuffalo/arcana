//! `arcana review` for a ledger vault: a terminal UI over pending agent
//! changes and unreviewed agent text.
//!
//! Keys: j/k move · a accept · r reject · c reject with a reason · e edit the
//! note yourself in $EDITOR · q quit. Every decision advances to the next item.

use anyhow::Result;
use arcana_core::attr::{Ledger, Pending, RawEdit, UnreviewedSpan};
use arcana_core::Vault;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use similar::{ChangeTag, TextDiff};

enum Item {
    Pending(Box<Pending>),
    Unreviewed(UnreviewedSpan),
}

impl Item {
    fn note(&self) -> &str {
        match self {
            Item::Pending(p) => &p.note,
            Item::Unreviewed(u) => &u.note,
        }
    }

    fn label(&self) -> String {
        let (tag, why) = match self {
            Item::Pending(p) => (
                if p.disposition == "suggest" {
                    "suggest"
                } else {
                    "change "
                },
                p.rationale
                    .clone()
                    .or(p.request.clone())
                    .unwrap_or_default(),
            ),
            Item::Unreviewed(u) => ("new    ", u.request.clone().unwrap_or_default()),
        };
        format!("{tag} {}  {why}", short_path(self.note()))
    }
}

struct App {
    items: Vec<Item>,
    list: ListState,
    status: String,
    /// Typing a rejection reason.
    reason: Option<String>,
    accepted: usize,
    rejected: usize,
}

pub fn run(vault: Vault) -> Result<()> {
    let mut app = App {
        items: Vec::new(),
        list: ListState::default(),
        status: String::new(),
        reason: None,
        accepted: 0,
        rejected: 0,
    };
    reload(&vault, &mut app)?;
    if app.items.is_empty() {
        eprintln!("nothing to review");
        return Ok(());
    }
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &vault, &mut app);
    ratatui::restore();
    eprintln!(
        "{} accepted · {} rejected · {} left",
        app.accepted,
        app.rejected,
        app.items.len()
    );
    result
}

fn ledger(vault: &Vault) -> &Ledger {
    vault
        .ledger()
        .expect("review_tui is only entered for ledger vaults")
}

fn reload(vault: &Vault, app: &mut App) -> Result<()> {
    let l = ledger(vault);
    let mut items: Vec<Item> = l
        .pending()?
        .into_iter()
        .map(|p| Item::Pending(Box::new(p)))
        .collect();
    items.extend(l.unreviewed()?.into_iter().map(Item::Unreviewed));
    app.items = items;
    let sel = app.list.selected().unwrap_or(0);
    app.list.select(if app.items.is_empty() {
        None
    } else {
        Some(sel.min(app.items.len() - 1))
    });
    Ok(())
}

fn event_loop(terminal: &mut DefaultTerminal, vault: &Vault, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| draw(f, app))?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if let Some(reason) = app.reason.as_mut() {
            match key.code {
                KeyCode::Enter => {
                    let reason = app.reason.take().filter(|r| !r.trim().is_empty());
                    decide(vault, app, false, reason)?;
                }
                KeyCode::Esc => app.reason = None,
                KeyCode::Backspace => {
                    reason.pop();
                }
                KeyCode::Char(c) => reason.push(c),
                _ => {}
            }
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Char('j') | KeyCode::Down => app.list.select_next(),
            KeyCode::Char('k') | KeyCode::Up => app.list.select_previous(),
            KeyCode::Char('a') => decide(vault, app, true, None)?,
            KeyCode::Char('r') => decide(vault, app, false, None)?,
            KeyCode::Char('c') => app.reason = Some(String::new()),
            KeyCode::Char('e') => {
                edit_myself(terminal, vault, app)?;
            }
            _ => {}
        }
        if app.items.is_empty() {
            return Ok(());
        }
    }
}

fn decide(vault: &Vault, app: &mut App, accept: bool, reason: Option<String>) -> Result<()> {
    let Some(i) = app.list.selected() else {
        return Ok(());
    };
    let l = ledger(vault);
    let git = vault.git();
    let result = match (&app.items[i], accept) {
        (Item::Pending(p), true) => l.accept(&p.id, git).map(|e| e.unwrap_or_default()),
        (Item::Pending(p), false) => l.reject(&p.id, reason).map(|_| String::new()),
        (Item::Unreviewed(u), keep) => l
            .review_span(&u.note, u.start, keep, git)
            .map(|e| e.unwrap_or_default()),
    };
    match result {
        Ok(warn) => {
            if accept {
                app.accepted += 1;
            } else {
                app.rejected += 1;
            }
            app.status = if warn.is_empty() {
                if accept { "accepted" } else { "rejected" }.into()
            } else {
                warn
            };
        }
        Err(e) => app.status = format!("error: {e}"),
    }
    reload(vault, app)
}

/// Edit the note in $EDITOR (a temp copy), then record the result as yours.
fn edit_myself(terminal: &mut DefaultTerminal, vault: &Vault, app: &mut App) -> Result<()> {
    let Some(i) = app.list.selected() else {
        return Ok(());
    };
    let note = app.items[i].note().to_string();
    let l = ledger(vault);
    let st = l.state(&note)?;
    let line = match &app.items[i] {
        Item::Unreviewed(u) => line_of(&st.content, u.start),
        Item::Pending(p) => {
            let anchor = match &p.edit {
                RawEdit::Replace { find, .. } | RawEdit::InsertAfter { find, .. } => find.clone(),
                RawEdit::Append { .. } => String::new(),
            };
            st.content
                .find(&anchor)
                .filter(|_| !anchor.is_empty())
                .map_or(1, |pos| line_of(&st.content, pos))
        }
    };
    let tmp = tempfile::Builder::new().suffix(".md").tempfile()?;
    std::fs::write(tmp.path(), &st.content)?;
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nvim".into());
    ratatui::restore();
    let status = std::process::Command::new(&editor)
        .arg(format!("+{line}"))
        .arg(tmp.path())
        .status();
    *terminal = ratatui::init();
    match status {
        Ok(s) if s.success() => {
            let new = std::fs::read_to_string(tmp.path())?;
            if new == st.content {
                app.status = "no changes".into();
            } else {
                let warn = l.editor_edit(&note, &new, vault.git())?;
                app.status = warn.unwrap_or_else(|| "saved your edit".into());
            }
        }
        Ok(_) => app.status = format!("{editor} exited with an error; nothing saved"),
        Err(e) => app.status = format!("could not run {editor}: {e}"),
    }
    reload(vault, app)
}

fn line_of(content: &str, byte: usize) -> usize {
    content[..byte.min(content.len())].matches('\n').count() + 1
}

fn short_path(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

fn draw(f: &mut Frame, app: &mut App) {
    let [main, footer] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(f.area());
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(34), Constraint::Fill(1)]).areas(main);

    let items: Vec<ListItem> = app.items.iter().map(|i| ListItem::new(i.label())).collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" review · {} ", app.items.len())),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, left, &mut app.list);

    let detail = app
        .list
        .selected()
        .and_then(|i| app.items.get(i))
        .map(detail_lines)
        .unwrap_or_default();
    let title = app
        .list
        .selected()
        .and_then(|i| app.items.get(i))
        .map(|i| format!(" {} ", i.note()))
        .unwrap_or_default();
    f.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(title)),
        right,
    );

    let help = match &app.reason {
        Some(r) => format!(" reason for rejecting (enter to send, esc to cancel): {r}▏"),
        None => format!(
            " a accept · r reject · c reject+why · e edit yourself · j/k move · q quit   {}",
            app.status
        ),
    };
    f.render_widget(
        Paragraph::new(help).style(Style::default().add_modifier(Modifier::DIM)),
        footer,
    );
}

fn header(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label:<9}"),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw(value.to_string()),
    ])
}

fn detail_lines(item: &Item) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match item {
        Item::Pending(p) => {
            let what = if p.disposition == "suggest" {
                "suggestion to your words"
            } else {
                "change to agent text"
            };
            lines.push(header("kind", what));
            lines.push(header("by", &p.agent));
            if let Some(r) = &p.request {
                lines.push(header("request", r));
            }
            if let Some(r) = &p.rationale {
                lines.push(header("why", r));
            }
            if p.disposition == "suggest" {
                if let Some(o) = arcana_core::attr::plan::light_edit(&p.before, &p.after) {
                    lines.push(header(
                        "light",
                        &format!("{} — accepting keeps these words yours", o.as_str()),
                    ));
                } else {
                    lines.push(header("light", "no — accepted words would be the agent's"));
                }
            }
            lines.push(Line::raw(""));
            let proposed = apply_preview(p);
            lines.extend(diff_lines(&p.context, &proposed));
        }
        Item::Unreviewed(u) => {
            lines.push(header("kind", "new agent text, already in the note"));
            lines.push(header("by", &u.agent));
            if let Some(r) = &u.request {
                lines.push(header("request", r));
            }
            lines.push(header("keep", "a keeps it (marks reviewed) · r removes it"));
            lines.push(Line::raw(""));
            for l in u.text.lines() {
                lines.push(Line::styled(
                    l.to_string(),
                    Style::default().fg(Color::Cyan),
                ));
            }
        }
    }
    lines
}

/// The proposal's context with the change applied, for diffing.
fn apply_preview(p: &Pending) -> String {
    match &p.edit {
        RawEdit::Replace { find, with } if p.context.contains(find.as_str()) => {
            p.context.replacen(find.as_str(), with, 1)
        }
        RawEdit::InsertAfter { find, text } if p.context.contains(find.as_str()) => p
            .context
            .replacen(find.as_str(), &format!("{find}{text}"), 1),
        _ => format!("{}{}", p.context, p.after),
    }
}

/// Word diff as styled lines: removed red and struck through, added green.
fn diff_lines(old: &str, new: &str) -> Vec<Line<'static>> {
    let diff = TextDiff::from_words(old, new);
    let mut lines = vec![Line::default()];
    for change in diff.iter_all_changes() {
        let style = match change.tag() {
            ChangeTag::Equal => Style::default(),
            ChangeTag::Delete => Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::CROSSED_OUT),
            ChangeTag::Insert => Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        };
        let mut parts = change.value().split('\n').peekable();
        while let Some(part) = parts.next() {
            if !part.is_empty() {
                if let Some(last) = lines.last_mut() {
                    last.spans.push(Span::styled(part.to_string(), style));
                }
            }
            if parts.peek().is_some() {
                lines.push(Line::default());
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn pending() -> Pending {
        Pending {
            id: "p1".into(),
            group: "g".into(),
            note: "writing/post.md".into(),
            created: chrono::Utc::now(),
            agent: "claude-code".into(),
            session: "s".into(),
            request: Some("fix my typos".into()),
            rationale: Some("typo".into()),
            disposition: "suggest".into(),
            edit: RawEdit::Replace {
                find: "teh resonance".into(),
                with: "the resonance".into(),
            },
            before: "teh resonance".into(),
            after: "the resonance".into(),
            context: "My thoughts on teh resonance and its sharpness.".into(),
        }
    }

    #[test]
    fn renders_a_suggestion_with_its_light_edit_status() {
        let mut app = App {
            items: vec![Item::Pending(Box::new(pending()))],
            list: ListState::default().with_selected(Some(0)),
            status: String::new(),
            reason: None,
            accepted: 0,
            rejected: 0,
        };
        let mut term = Terminal::new(TestBackend::new(120, 20)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let screen: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(screen.contains("review · 1"));
        assert!(screen.contains("suggestion to your words"));
        assert!(screen.contains("accepting keeps these words yours"));
        assert!(screen.contains("teh"), "old word shown struck through");
        assert!(screen.contains("the resonance"));
    }

    #[test]
    fn preview_applies_insertions_in_context() {
        let mut p = pending();
        p.edit = RawEdit::InsertAfter {
            find: "sharpness".into(),
            text: " [@french1971]".into(),
        };
        assert_eq!(
            apply_preview(&p),
            "My thoughts on teh resonance and its sharpness [@french1971]."
        );
    }
}
