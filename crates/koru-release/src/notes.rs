// SPDX-License-Identifier: MPL-2.0

//! Generate a missing release descriptor — or a GitHub nightly's notes — with a
//! chosen assistant, then review it.

use crate::{confirm, read_answer, shell};
use koru_update::ReleaseChannel;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

// There is no default generator: with `KORU_NOTES_COMMAND` unset the publisher
// offers only the built-in assistants below, or stops so the descriptor can be
// written by hand.
//
// `exec` reads the prompt from stdin. Workspace writes create the descriptor;
// network access lets it read the published manifests named in the instructions.
const CODEX_COMMAND: &str =
    "codex exec --sandbox workspace-write -c sandbox_workspace_write.network_access=true -";
// Plain instructions. The built-in assistants receive the content directly, and
// a custom `KORU_NOTES_COMMAND` generator can read the same file.
const WRITING_INSTRUCTIONS: &str = include_str!("../../../releases/WRITING.md");

struct NotesCommand {
    command: String,
    input: Option<PromptInput>,
}

/// What the generator is asked to write.
#[derive(Clone, Copy)]
pub(crate) enum Request<'a> {
    /// The descriptor `dist-cloud` is about to publish for a channel.
    Release(ReleaseChannel),
    /// A GitHub nightly's notes file, describing the commits `log` lists
    /// (`nightly-notes`; WRITING.md's *Nightly notes on GitHub*).
    Nightly { log: &'a str },
}

impl Request<'_> {
    fn channel(&self) -> ReleaseChannel {
        match self {
            Request::Release(channel) => *channel,
            Request::Nightly { .. } => ReleaseChannel::Nightly,
        }
    }

    /// What the assistant is asked, ahead of the shared instructions.
    fn preface(&self, path: &Path) -> String {
        match self {
            Request::Release(channel) => format!(
                "Generate release notes for the {channel} channel.\n\
                 The publisher has already selected the version. Write exactly this \
                 descriptor, even if the UTC date or published versions have changed:\n\
                 {path}\n",
                channel = channel.as_str(),
                path = path.display(),
            ),
            Request::Nightly { log } => format!(
                "Generate the notes for Koru's GitHub nightly.\n\
                 Write exactly this notes file, even if the UTC date has changed:\n\
                 {path}\n\
                 It describes the commits `{log}` lists. Follow the section \
                 \"Nightly notes on GitHub\" of the instructions below.\n",
                path = path.display(),
            ),
        }
    }
}

enum PromptInput {
    Stdin(String),
    Argument(String),
}

impl NotesCommand {
    fn new(template: &str, request: Request<'_>, descriptor_path: &Path) -> Self {
        let assistant = template.trim().to_ascii_lowercase();
        if matches!(assistant.as_str(), "codex" | "kimi") {
            // Kimi Code 0.42.0 takes --prompt as an argument, not stdin.
            // Both transports keep checkout paths and prose out of shell syntax.
            let prompt = format!(
                "{preface}Follow the release-writing instructions below.\n\n{WRITING_INSTRUCTIONS}",
                preface = request.preface(descriptor_path),
            );
            let (command, input) = if assistant == "kimi" {
                ("kimi", PromptInput::Argument(prompt))
            } else {
                (CODEX_COMMAND, PromptInput::Stdin(prompt))
            };
            Self {
                command: command.into(),
                input: Some(input),
            }
        } else {
            // `{path}` is the exact file to write, which a custom generator
            // cannot infer for a nightly's `-2`/`-3`. It is substituted as it
            // is, unquoted — the template's author quotes it if a checkout
            // path may hold spaces.
            Self {
                command: template
                    .replace("{channel}", request.channel().as_str())
                    .replace("{path}", &descriptor_path.display().to_string()),
                input: None,
            }
        }
    }

    fn invocation(&self) -> Command {
        if let Some(PromptInput::Argument(prompt)) = &self.input {
            // Launch the executable directly so a prompt containing quotes,
            // newlines, or shell operators remains one literal argument.
            let mut invocation = Command::new(&self.command);
            invocation.arg("--prompt").arg(prompt);
            invocation
        } else {
            shell(&self.command)
        }
    }

    fn run(&self, root: &Path) -> Result<(), String> {
        let mut invocation = self.invocation();
        invocation.current_dir(root);
        let status = if let Some(PromptInput::Stdin(prompt)) = &self.input {
            let mut child = invocation
                .stdin(Stdio::piped())
                .spawn()
                .map_err(|error| format!("could not start the release-notes command: {error}"))?;
            // Drop stdin to signal EOF, and reap the child even if writing fails.
            let sent = child
                .stdin
                .take()
                .expect("piped stdin")
                .write_all(prompt.as_bytes());
            let status = child.wait().map_err(|error| error.to_string())?;
            if status.success() {
                sent.map_err(|error| {
                    format!("could not send release-notes instructions: {error}")
                })?;
            }
            status
        } else {
            invocation
                .status()
                .map_err(|error| format!("could not start the release-notes command: {error}"))?
        };
        if status.success() {
            Ok(())
        } else {
            Err("the release-notes command failed; run again and choose codex or kimi, or set KORU_NOTES_COMMAND to another generator".into())
        }
    }
}

/// The generator an answer picks: `y` takes the configured command, if there
/// is one; `codex` and `kimi` are always on offer.
fn chosen_template<'a>(answer: &str, configured: Option<&'a str>) -> Option<&'a str> {
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => configured,
        "codex" => Some("codex"),
        "kimi" => Some("kimi"),
        _ => None,
    }
}

/// Generated copy is never published unseen: no assistant or `--yes`
/// can skip the review of text that becomes an immutable release page.
pub(crate) fn offer_generated_notes(
    root: &Path,
    channel: ReleaseChannel,
    descriptor_path: &Path,
    yes: bool,
) -> Result<(), String> {
    if yes {
        return Err(format!(
            "create {} before publishing; --yes cannot accept generated notes unseen",
            descriptor_path.display()
        ));
    }
    println!("[Koru] no descriptor at {}", descriptor_path.display());
    let notes = generate(root, Request::Release(channel), descriptor_path)?;
    println!("\n--- {} ---", descriptor_path.display());
    println!("{notes}");
    println!("--- end ---\n");
    if confirm("Use these release notes? [y/N] ")? {
        Ok(())
    } else {
        Err(format!(
            "release notes rejected; edit {} and run again, or delete it to regenerate",
            descriptor_path.display()
        ))
    }
}

/// Ask which generator to use, run it, and return what it wrote at `path`.
///
/// No default: with `KORU_NOTES_COMMAND` unset only a named assistant runs,
/// and any other answer stops so the file can be written by hand.
pub(crate) fn generate(root: &Path, request: Request<'_>, path: &Path) -> Result<String, String> {
    let configured = std::env::var("KORU_NOTES_COMMAND")
        .ok()
        .filter(|template| !template.trim().is_empty());
    let question = match &configured {
        Some(template) => format!(
            "Generate one with `{}`? [y/N, or codex/kimi] ",
            NotesCommand::new(template, request, path).command
        ),
        None => "Generate one with an assistant? [codex/kimi, or N to write it yourself] ".into(),
    };
    let answer = read_answer(&question)?;
    let template =
        chosen_template(&answer, configured.as_deref()).ok_or_else(|| match request {
            Request::Release(_) => format!("create {} before publishing", path.display()),
            Request::Nightly { .. } => format!("write {} yourself", path.display()),
        })?;
    let selected = NotesCommand::new(template, request, path);
    println!(
        "[Koru] generating release notes with `{}`",
        selected.command
    );
    selected.run(root)?;

    // Custom generators can still land a neighboring filename. Read only the
    // exact file selected by this run, never whichever appeared.
    fs::read_to_string(path).map_err(|_| {
        format!(
            "the release-notes command did not write {}; check releases/ for a near-miss filename",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests;
