use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use omarchy_whatsapp_protocol::{AppPaths, Chat, ClientFrame, Command, ServerEvent, ServerFrame};
use serde_json::json;
use std::fmt::Write as FmtWrite;
use std::fs::{self, OpenOptions, Permissions};
use std::io::Write as IoWrite;
use std::ops::Range;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const MENU_BLOCK_BEGIN: &str = "  // BEGIN omarchy-whatsapp launcher chats (generated)";
const MENU_BLOCK_END: &str = "  // END omarchy-whatsapp launcher chats";
const WHATSAPP_MENU_ICON: &str = "\u{f05a3}";
const PLUGIN_ID: &str = "io.github.bryantebeek.whatsapp";

#[derive(Debug, Parser)]
#[command(version, about = "Control and inspect the Omarchy WhatsApp daemon")]
struct Options {
    #[arg(long)]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    command: Action,
}

#[derive(Debug, Subcommand)]
enum Action {
    Status,
    Chats {
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    Messages {
        chat: String,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    Send {
        chat: String,
        text: String,
    },
    MarkRead {
        chat: String,
    },
    /// Add current conversations to Omarchy's Super+Space search.
    LauncherSync {
        #[arg(long, default_value_t = 500)]
        limit: u32,
        #[arg(long, hide = true)]
        menu_path: Option<PathBuf>,
    },
    /// Remove generated WhatsApp conversations from Omarchy's menu.
    LauncherRemove {
        #[arg(long, hide = true)]
        menu_path: Option<PathBuf>,
    },
    Ping,
}

#[tokio::main]
async fn main() -> Result<()> {
    let options = Options::parse();
    let socket = options
        .socket
        .unwrap_or_else(|| AppPaths::discover().socket);

    match options.command {
        Action::LauncherSync { limit, menu_path } => {
            let event = request(&socket, Command::ListChats { limit }).await?;
            let ServerEvent::Chats { chats } = event else {
                bail!("daemon returned an unexpected response to list_chats")
            };
            let path = menu_path.unwrap_or(menu_path_from_environment()?);
            let changed = sync_launcher_menu(&path, &chats)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "launcher": "synced",
                    "chats": chats.len(),
                    "changed": changed,
                    "path": path,
                }))?
            );
            return Ok(());
        }
        Action::LauncherRemove { menu_path } => {
            let path = menu_path.unwrap_or(menu_path_from_environment()?);
            let changed = remove_launcher_menu(&path)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "launcher": "removed",
                    "changed": changed,
                    "path": path,
                }))?
            );
            return Ok(());
        }
        action => {
            let event = request(&socket, command_for_action(action)?).await?;
            println!("{}", serde_json::to_string_pretty(&event)?);
        }
    }
    Ok(())
}

fn command_for_action(action: Action) -> Result<Command> {
    Ok(match action {
        Action::Status => Command::GetState,
        Action::Chats { limit } => Command::ListChats { limit },
        Action::Messages { chat, limit } => Command::GetMessages {
            chat_jid: chat,
            limit,
        },
        Action::Send { chat, text } => Command::SendMessage {
            chat_jid: chat,
            text,
        },
        Action::MarkRead { chat } => Command::MarkRead { chat_jid: chat },
        Action::Ping => Command::Ping,
        Action::LauncherSync { .. } | Action::LauncherRemove { .. } => {
            bail!("launcher actions are handled before daemon command dispatch")
        }
    })
}

async fn request(socket: &Path, command: Command) -> Result<ServerEvent> {
    let stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connecting to {}", socket.display()))?;
    let (read, mut write) = stream.into_split();
    let request = ClientFrame::new(Some(1), command);
    write.write_all(&serde_json::to_vec(&request)?).await?;
    write.write_all(b"\n").await?;

    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        let response: ServerFrame = serde_json::from_str(&line)?;
        if response.id == Some(1) {
            return Ok(response.event);
        }
    }
    bail!("daemon disconnected before responding")
}

fn menu_path_from_environment() -> Result<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .context("neither XDG_CONFIG_HOME nor HOME is set")?;
    Ok(config
        .join("omarchy")
        .join("extensions")
        .join("omarchy-menu.jsonc"))
}

fn sync_launcher_menu(path: &Path, chats: &[Chat]) -> Result<bool> {
    let existing = read_menu_or_default(path)?;
    let updated = upsert_menu_block(&existing, &render_menu_block(chats))?;
    write_if_changed(path, &existing, &updated)
}

fn remove_launcher_menu(path: &Path) -> Result<bool> {
    let existing = match fs::read_to_string(path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let Some(span) = generated_block_span(&existing)? else {
        return Ok(false);
    };
    let mut updated = existing.clone();
    updated.replace_range(span, "");
    write_if_changed(path, &existing, &updated)
}

fn read_menu_or_default(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(value) => Ok(value),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok("{\n}\n".to_owned()),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

fn upsert_menu_block(existing: &str, block: &str) -> Result<String> {
    let replacement = format!("\n{block}");
    if let Some(span) = generated_block_span(existing)? {
        let mut updated = existing.to_owned();
        updated.replace_range(span, &replacement);
        return Ok(updated);
    }

    let open_brace = root_object_open(existing)
        .context("Omarchy menu extension is not a JSONC object; refusing to overwrite it")?;
    let mut updated = existing.to_owned();
    updated.insert_str(open_brace + 1, &replacement);
    Ok(updated)
}

fn generated_block_span(text: &str) -> Result<Option<Range<usize>>> {
    let Some(begin) = text.find(MENU_BLOCK_BEGIN) else {
        if text.contains(MENU_BLOCK_END) {
            bail!("found WhatsApp launcher end marker without a begin marker")
        }
        return Ok(None);
    };
    if text[begin + MENU_BLOCK_BEGIN.len()..].contains(MENU_BLOCK_BEGIN) {
        bail!("found more than one WhatsApp launcher block")
    }
    let end = text[begin + MENU_BLOCK_BEGIN.len()..]
        .find(MENU_BLOCK_END)
        .map(|offset| begin + MENU_BLOCK_BEGIN.len() + offset)
        .context("found WhatsApp launcher begin marker without an end marker")?;
    if text[end + MENU_BLOCK_END.len()..].contains(MENU_BLOCK_END) {
        bail!("found more than one WhatsApp launcher end marker")
    }
    let start = text[..begin].rfind('\n').unwrap_or(begin);
    Ok(Some(start..end + MENU_BLOCK_END.len()))
}

fn root_object_open(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b' ' | b'\t' | b'\r' | b'\n' => index += 1,
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'{' => return Some(index),
            _ => return None,
        }
    }
    None
}

fn render_menu_block(chats: &[Chat]) -> String {
    let mut block = String::new();
    writeln!(block, "{MENU_BLOCK_BEGIN}").expect("writing to a string cannot fail");
    push_menu_entry(
        &mut block,
        "whatsapp",
        json!({
            "icon": WHATSAPP_MENU_ICON,
            "label": "WhatsApp",
            "description": "Search conversations",
        }),
    );
    push_menu_entry(
        &mut block,
        "wa-open",
        json!({
            "parent": "whatsapp",
            "icon": WHATSAPP_MENU_ICON,
            "label": "Open WhatsApp",
            "action": format!("omarchy shell -q {PLUGIN_ID} open"),
        }),
    );

    for chat in chats {
        let id = format!("wa-chat.{}", hex_id(&chat.jid));
        let label = if chat.name.trim().is_empty() {
            chat.jid.as_str()
        } else {
            chat.name.trim()
        };
        let action = format!(
            "omarchy shell -q {PLUGIN_ID} openChat {}",
            shell_quote(&chat.jid)
        );
        push_menu_entry(
            &mut block,
            &id,
            json!({
                "parent": "whatsapp",
                "icon": WHATSAPP_MENU_ICON,
                "label": label,
                "description": if chat.is_group { "Group" } else { "Contact" },
                "action": action,
            }),
        );
    }

    block.push_str(MENU_BLOCK_END);
    block
}

fn push_menu_entry(block: &mut String, id: &str, value: serde_json::Value) {
    writeln!(
        block,
        "  {}: {},",
        serde_json::to_string(id).expect("serializing a string cannot fail"),
        serde_json::to_string(&value).expect("serializing a JSON value cannot fail")
    )
    .expect("writing to a string cannot fail");
}

fn hex_id(value: &str) -> String {
    let mut output = String::with_capacity(value.len() * 2);
    for byte in value.as_bytes() {
        write!(output, "{byte:02x}").expect("writing to a string cannot fail");
    }
    output
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn write_if_changed(path: &Path, existing: &str, updated: &str) -> Result<bool> {
    if existing == updated {
        return Ok(false);
    }
    let parent = path
        .parent()
        .context("Omarchy menu extension path has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let permissions = fs::metadata(path)
        .map(|metadata| metadata.permissions())
        .unwrap_or_else(|_| Permissions::from_mode(0o644));

    let mut temporary = None;
    for attempt in 0..100_u32 {
        let candidate = parent.join(format!(
            ".{}.omarchy-whatsapp.{}.{}.tmp",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("menu"),
            std::process::id(),
            attempt
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&candidate)
        {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("creating temporary file in {}", parent.display()));
            }
        }
    }
    let (temporary_path, mut file) =
        temporary.context("could not allocate a temporary menu file")?;
    let result = (|| -> Result<()> {
        file.write_all(updated.as_bytes())?;
        file.sync_all()?;
        file.set_permissions(permissions)?;
        drop(file);
        fs::rename(&temporary_path, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result.with_context(|| format!("atomically updating {}", path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn chat(jid: &str, name: &str, is_group: bool) -> Chat {
        Chat {
            jid: jid.to_owned(),
            name: name.to_owned(),
            phone_number: None,
            last_message: "private message text must not be indexed".to_owned(),
            last_timestamp: 123,
            unread: 4,
            is_group,
        }
    }

    #[test]
    fn launcher_block_escapes_remote_names_and_shell_arguments() {
        let block = render_menu_block(&[chat("odd'jid@s.whatsapp.net", "Ada \"QA\"\nTeam", false)]);
        assert!(block.contains("Ada \\\"QA\\\"\\nTeam"));
        assert!(block.contains("'odd'\\\\''jid@s.whatsapp.net'"));
        assert!(!block.contains("private message text"));
    }

    #[test]
    fn launcher_uses_one_whatsapp_icon_for_every_result() {
        let block = render_menu_block(&[
            chat("1@s.whatsapp.net", "Ada", false),
            chat("2@g.us", "Family", true),
        ]);
        let icon = format!(
            "\"icon\":{}",
            serde_json::to_string(WHATSAPP_MENU_ICON).unwrap()
        );
        assert_eq!(block.matches(&icon).count(), 4);
        assert!(block.contains("\"description\":\"Contact\""));
        assert!(block.contains("\"description\":\"Group\""));
    }

    #[test]
    fn sync_updates_only_its_marked_block_and_remove_restores_the_file() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("omarchy-menu.jsonc");
        let original = "{\n  // Mine\n  \"personal\": {\"label\":\"Personal\"}\n}\n";
        fs::write(&path, original).unwrap();

        assert!(sync_launcher_menu(&path, &[chat("1@s.whatsapp.net", "Ada", false)]).unwrap());
        let first = fs::read_to_string(&path).unwrap();
        assert!(first.contains(MENU_BLOCK_BEGIN));
        assert!(first.contains("\"label\":\"Ada\""));
        assert!(first.contains("\"personal\""));

        assert!(sync_launcher_menu(&path, &[chat("1@s.whatsapp.net", "Grace", false)]).unwrap());
        let second = fs::read_to_string(&path).unwrap();
        assert_eq!(second.matches(MENU_BLOCK_BEGIN).count(), 1);
        assert!(second.contains("\"label\":\"Grace\""));
        assert!(!second.contains("\"label\":\"Ada\""));
        assert!(!sync_launcher_menu(&path, &[chat("1@s.whatsapp.net", "Grace", false)]).unwrap());

        assert!(remove_launcher_menu(&path).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert!(!remove_launcher_menu(&path).unwrap());
    }

    #[test]
    fn sync_refuses_to_replace_an_unclosed_generated_block() {
        let error = upsert_menu_block(
            &format!("{{\n{MENU_BLOCK_BEGIN}\n}}\n"),
            &render_menu_block(&[]),
        )
        .unwrap_err();
        assert!(error.to_string().contains("without an end marker"));
    }
}
