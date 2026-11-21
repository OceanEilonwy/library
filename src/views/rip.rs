// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pages for Library Rip: connecting it (`/connect`, `/connect/device`),
//! downloading it (`/ripper`) and the desk's list of connected rippers.

use maud::{Markup, html};

use super::{Ctx, Header, date_long, page};
use crate::db::api_keys::ApiKey;
use crate::views::admin::size_label;

/// A connect request that passed every check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectRequest {
    pub name: String,
    pub redirect_uri: String,
    pub state: String,
    pub code_challenge: String,
}

/// "Connect Henry's desktop (Library Rip) to The Library?"
pub fn connect_confirm(ctx: &Ctx, r: &ConnectRequest) -> Markup {
    page(
        ctx,
        "Connect Library Rip",
        Header::Plain,
        html! {
            form.unlock-page.connect method="post" action="/connect" {
                h1.page-title { "Connect Library Rip" }
                p.connect-question {
                    "Connect " strong { (r.name) } " (Library Rip) to " (ctx.branding.title) "?"
                }
                p {
                    "It will be able to add items and upload files. You can disconnect it at any time from the "
                    a href="/admin" { "Librarian's desk" } "."
                }
                input type="hidden" name="client" value=(crate::web::rip::CLIENT);
                input type="hidden" name="name" value=(r.name);
                input type="hidden" name="redirect_uri" value=(r.redirect_uri);
                input type="hidden" name="state" value=(r.state);
                input type="hidden" name="code_challenge" value=(r.code_challenge);
                input type="hidden" name="code_challenge_method" value="S256";
                div.row-actions {
                    button.btn.solid type="submit" name="decision" value="yes" autofocus { "Yes, connect" }
                    button.btn type="submit" name="decision" value="no" { "No" }
                }
                p.small.muted { "Only say yes if you started connecting from Library Rip just now." }
            }
        },
    )
}

/// A connect link that failed a check.
pub fn connect_problem(ctx: &Ctx, message: &str) -> Markup {
    page(
        ctx,
        "Connect Library Rip",
        Header::Plain,
        html! {
            div.unlock-page {
                h1.page-title { "This link can't be used" }
                p.flash.err role="alert" { (message) }
                p { "Start connecting again from Library Rip, or connect it with a code instead." }
                a.btn href="/connect/device" { "Connect with a code" }
            }
        },
    )
}

/// The form where the librarian types the code shown by Library Rip.
pub fn device_form(ctx: &Ctx, code: &str, error: Option<&str>) -> Markup {
    page(
        ctx,
        "Connect Library Rip",
        Header::Plain,
        html! {
            form.unlock-page.connect method="post" action="/connect/device" {
                h1.page-title { "Connect Library Rip with a code" }
                p { "Library Rip shows a code like " span.mono { "WXYZ-1234" } ". Type it here." }
                @if let Some(e) = error { p.flash.err role="alert" { (e) } }
                div.field {
                    label for="device-code" { "Code" }
                    input #device-code.u-input.big.mono name="code" value=(code) maxlength="12"
                        autocomplete="off" autocapitalize="characters" spellcheck="false" required autofocus;
                }
                div.row-actions {
                    button.btn.solid type="submit" { "Continue" }
                    a.btn href="/admin" { "Cancel" }
                }
            }
        },
    )
}

/// "Connect Henry's laptop (Library Rip)?" for a typed code.
pub fn device_confirm(ctx: &Ctx, name: &str, user_code: &str) -> Markup {
    page(
        ctx,
        "Connect Library Rip",
        Header::Plain,
        html! {
            form.unlock-page.connect method="post" action="/connect/device" {
                h1.page-title { "Connect Library Rip" }
                p.connect-question {
                    "Connect " strong { (name) } " (Library Rip) to " (ctx.branding.title) "?"
                }
                p {
                    "It will be able to add items and upload files. You can disconnect it at any time from the "
                    a href="/admin" { "Librarian's desk" } "."
                }
                p.small.muted { "Code " span.mono { (user_code) } }
                input type="hidden" name="code" value=(user_code);
                div.row-actions {
                    button.btn.solid type="submit" name="decision" value="yes" autofocus { "Yes, connect" }
                    button.btn type="submit" name="decision" value="no" { "No" }
                }
            }
        },
    )
}

/// After answering a typed code.
pub fn device_done(ctx: &Ctx, name: &str, approved: bool) -> Markup {
    page(
        ctx,
        "Connect Library Rip",
        Header::Plain,
        html! {
            div.unlock-page {
                @if approved {
                    h1.page-title { "Connected" }
                    p { strong { (name) } " is connected. Library Rip will carry on by itself in a few seconds." }
                } @else {
                    h1.page-title { "Not connected" }
                    p { strong { (name) } " was not connected. Library Rip will say so." }
                }
                a.btn href="/admin" { "Back to the Librarian's desk" }
            }
        },
    )
}

/// The desk's "Connected rippers" section.
pub fn connected_rippers(keys: &[ApiKey]) -> Markup {
    html! {
        section #rippers aria-labelledby="rippers-title" {
            h2 #rippers-title .kicker { "Connected rippers" }
            @if keys.is_empty() {
                p.muted.small {
                    "No computers are connected. "
                    a href="/ripper" { "Get Library Rip" }
                    " to copy CDs into the library."
                }
            } @else {
                ul.admin-list.rippers {
                    @for k in keys {
                        li #{ "ripper-" (k.id) } {
                            div.grow {
                                strong.serif { (k.name) }
                                span.small.muted {
                                    " · connected " (date_long(k.created_at))
                                    " · "
                                    @match k.last_used_at {
                                        Some(t) => { "last used " (date_long(t)) }
                                        None => { "not used yet" }
                                    }
                                }
                            }
                            form method="post" action={ "/admin/rippers/" (k.id) "/disconnect" }
                                data-confirm={ "Disconnect " (k.name) "? It will have to be connected again before it can upload." } {
                                button.link-btn.small type="submit" aria-label={ "Disconnect " (k.name) } { "Disconnect" }
                            }
                        }
                    }
                }
            }
            p.small {
                a href="/connect/device" { "Connect a ripper with a code" }
                " · "
                a href="/ripper" { "Get Library Rip" }
            }
        }
    }
}

/// Operating systems the ripper is built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Os {
    Linux,
    Mac,
    Windows,
    Other,
}

impl Os {
    pub fn label(self) -> &'static str {
        match self {
            Os::Linux => "Linux",
            Os::Mac => "macOS",
            Os::Windows => "Windows",
            Os::Other => "Other",
        }
    }

    /// The visitor's system, from their browser's User-Agent.
    pub fn from_user_agent(ua: &str) -> Option<Self> {
        if ua.contains("iPhone") || ua.contains("iPad") || ua.contains("Android") {
            None
        } else if ua.contains("Windows") {
            Some(Os::Windows)
        } else if ua.contains("Macintosh") || ua.contains("Mac OS X") {
            Some(Os::Mac)
        } else if ua.contains("Linux") || ua.contains("X11") || ua.contains("CrOS") {
            Some(Os::Linux)
        } else {
            None
        }
    }

    /// Which system a build file is for, from its name.
    pub fn of_build(file: &str) -> Self {
        let f = file.to_ascii_lowercase();
        if f.contains("linux") || f.ends_with(".appimage") {
            Os::Linux
        } else if f.ends_with(".dmg") || f.contains("macos") || f.contains("darwin") {
            Os::Mac
        } else if f.contains("windows")
            || f.contains("win64")
            || f.ends_with(".exe")
            || f.ends_with(".msi")
            || f.ends_with(".zip")
        {
            Os::Windows
        } else {
            Os::Other
        }
    }
}

/// A file offered on `/ripper`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    pub file: String,
    pub os: Os,
    pub size: u64,
    pub sha256: String,
}

/// What `/ripper` shows.
pub struct RipperView<'a> {
    pub builds: &'a [Build],
    pub visitor: Option<Os>,
    pub source_url: &'a str,
}

fn build_list(builds: &[&Build]) -> Markup {
    html! {
        ul.admin-list.builds {
            @for b in builds {
                li {
                    div.grow {
                        a href={ "/ripper/download/" (b.file) } download { strong { (b.file) } }
                        span.small.muted { " · " (b.os.label()) " · " (size_label(i64::try_from(b.size).unwrap_or(i64::MAX))) }
                        @if !b.sha256.is_empty() {
                            div.small.muted.mono.sha { "SHA-256 " (b.sha256) }
                        }
                    }
                }
            }
        }
    }
}

/// The "Get Library Rip" page.
pub fn ripper_page(ctx: &Ctx, v: &RipperView) -> Markup {
    let (yours, others): (Vec<&Build>, Vec<&Build>) = v
        .builds
        .iter()
        .partition(|b| v.visitor.is_some_and(|os| b.os == os));
    page(
        ctx,
        "Get Library Rip",
        Header::Plain,
        html! {
            h1.page-title { "Get Library Rip" }
            p.page-sub {
                "Library Rip copies your CDs into the library: an exact, checked copy of every track, "
                "with the album's details and cover filled in from MusicBrainz."
            }
            div.two-col {
                section.left aria-labelledby="builds-title" {
                    h2 #builds-title .kicker { "Download" }
                    @if v.builds.is_empty() {
                        p.muted {
                            "This copy of the library came without a Library Rip build. Run "
                            span.mono { "scripts/bundle-rip.sh" } " to add one, or copy builds into the "
                            span.mono { "downloads/library-rip" } " folder of the library's data folder "
                            "(or the folder set by " span.mono { "[ripper] downloads_dir" } ")."
                        }
                    } @else {
                        @if let Some(os) = v.visitor && !yours.is_empty() {
                            h3.small.caps { "For " (os.label()) }
                            (build_list(&yours))
                            @if !others.is_empty() { h3.small.caps { "Other systems" } }
                        }
                        @if yours.is_empty() || v.visitor.is_none() {
                            (build_list(&v.builds.iter().collect::<Vec<_>>()))
                        } @else {
                            (build_list(&others))
                        }
                        p.small.muted {
                            "Check a download's SHA-256 matches before running it ("
                            span.mono { "sha256sum" } " on Linux, " span.mono { "shasum -a 256" } " on macOS, "
                            span.mono { "Get-FileHash" } " on Windows)."
                        }
                    }
                    h2.kicker { "Connecting it" }
                    p {
                        "Start Library Rip, type this library's address and press Connect. Your browser opens "
                        "here and asks you to say yes. If the browser is on another device, choose "
                        "“Use a code instead” and type the code on "
                        a href="/connect/device" { "Connect with a code" } "."
                    }
                    p.small.muted {
                        "Library Rip is free software under the GNU AGPL, like this library. "
                        a href=(v.source_url) rel="noopener" { "Source code" } "."
                    }
                }
                section.right aria-labelledby="films-title" {
                    h2 #films-title .kicker { "Films (DVDs)" }
                    p {
                        "Music CDs need nothing else. For a film disc, Library Rip copies the film with MakeMKV's "
                        span.mono { "makemkvcon" } " into a lossless MKV (kept as an archive file on the item page), "
                        "and makes a WebM copy the Player can play with " span.mono { "ffmpeg" } ". "
                        "Install them yourself; Library Rip finds them on its own."
                    }
                    h3.small.caps { "MakeMKV" }
                    dl.makemkv {
                        dt { "Linux" }
                        dd {
                            "From Flathub: " span.mono { "flatpak install flathub com.makemkv.MakeMKV" }
                            ", or build it from the tarballs linked on the MakeMKV forum "
                            "(makemkv-oss and makemkv-bin)."
                        }
                        dt { "Windows" }
                        dd { "Run the Windows installer from " a href="https://www.makemkv.com/download/" rel="noopener" { "makemkv.com" } "." }
                        dt { "macOS" }
                        dd { "Open the macOS disk image from " a href="https://www.makemkv.com/download/" rel="noopener" { "makemkv.com" } " and drag MakeMKV into Applications." }
                    }
                    p.small.muted {
                        "On Linux MakeMKV also needs the " span.mono { "sg" } " kernel module "
                        "(" span.mono { "sudo modprobe sg" } "), and your user must be allowed to use the drive."
                    }
                    h3.small.caps { "ffmpeg (for the viewing copy)" }
                    p {
                        "Any ffmpeg with an AV1 encoder (" span.mono { "libsvtav1" } " or " span.mono { "libaom-av1" }
                        ") and " span.mono { "libopus" } ": "
                        span.mono { "sudo apt install ffmpeg" } " or " span.mono { "sudo pacman -S ffmpeg" }
                        " on Linux, " span.mono { "brew install ffmpeg" } " on macOS, "
                        a href="https://www.gyan.dev/ffmpeg/builds/" rel="noopener" { "gyan.dev" }
                        " or " span.mono { "winget install ffmpeg" } " on Windows. Without it Library Rip "
                        "archives the master only, and the Player has nothing to play yet."
                    }
                    p.small.muted {
                        "DVDs are what Library Rip has been tried with. Blu-ray discs are passed to MakeMKV too, "
                        "but that is untested, and MakeMKV needs your own registration key for them. "
                        "Breaking the copy protection on discs is restricted by law in many countries, "
                        "so check what is allowed where you live."
                    }
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operating_systems() {
        let ua = |s| Os::from_user_agent(s);
        assert_eq!(
            ua("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36"),
            Some(Os::Windows)
        );
        assert_eq!(
            ua("Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) AppleWebKit/605.1.15"),
            Some(Os::Mac)
        );
        assert_eq!(
            ua("Mozilla/5.0 (X11; Linux x86_64; rv:132.0) Gecko/20100101 Firefox/132.0"),
            Some(Os::Linux)
        );
        assert_eq!(ua("Mozilla/5.0 (Linux; Android 14; Pixel 8)"), None);
        assert_eq!(
            ua("Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X)"),
            None
        );
        assert_eq!(ua(""), None);

        assert_eq!(Os::of_build("library-rip-0.1.0-x86_64.AppImage"), Os::Linux);
        assert_eq!(
            Os::of_build("library-rip-0.1.0-linux-x86_64.tar.gz"),
            Os::Linux
        );
        assert_eq!(Os::of_build("library-rip-0.1.0-universal.dmg"), Os::Mac);
        assert_eq!(
            Os::of_build("library-rip-0.1.0-windows-x64.zip"),
            Os::Windows
        );
        assert_eq!(Os::of_build("notes.txt"), Os::Other);
    }
}
