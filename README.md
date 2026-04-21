> [!IMPORTANT]
> This is an *alpha-quality hobby project*. I do use this
> tool myself, but I started this project mainly to learn rust. While I
> appreciate community input, I don't have much extra time to spend on this and
> I'll be unresponsive to issue reports. I will however happily merge PRs with
> improvements.

# lifter

Would you like to automatically download cool binaries like ripgrep,
fzf, bat, as soon as a new version is posted on their Github Releases
pages? And would you like this to work for all websites where
such binaries are posted, not only Github?

*lifter* is a CLI tool for downloading single-file executables
from sites like Github that make them available, *and only downloading
newer versions when available*.

Nearly all projects that make CLI tools, like say _ripgrep_,
put those binary artifacts in Github releases; but then we
have to wait until someone packages those binaries
into various OS distro package managers so that we can
get them via _apt_ or _yum_ or _chocolatey_.  No more
waiting! _lifter_ will download directly from the
Github Releases page, if there is a new version released.

### Why the name?

```
$ dictomatic lifter
lifter  noun    a thief who steals goods that are in a store
lifter  noun    an athlete who lifts barbells
```

Take your pick.  (By the way, `dictomatic` is another one of my
hobby projects, and `lifter` will download that binary for you
too.)

## Demo

Requires the presence of a `lifter.config` file alongside the binary. You can
use the example one in this repo.

```bash
$ ls -lh | rg lifter
.rwxrwxr-x  6.9M caleb  2 Apr 12:27  lifter
.rw-rw-r--   14k caleb  2 Apr 14:41  lifter.config
$ ./lifter
2026-04-21T14:23:45Z,0,thesauromatic.exe,thesauromatic.exe,Alpha,Alpha
2026-04-21T14:23:46Z,0,tokei,tokei,v12.1.2,v12.1.2
2026-04-21T14:23:46Z,0,ncspot,ncspot,v0.7.3,v0.7.3
2026-04-21T14:23:47Z,0,starship.exe,starship.exe,v0.55.0,v0.55.0
2026-04-21T14:23:47Z,0,caddy,caddy,v2.4.3,v2.4.3
2026-04-21T14:23:48Z,0,gitea,gitea,v1.14.3,v1.14.3
2026-04-21T14:23:49Z,1,ripgrep,rg,13.0.0,14.1.0
2026-04-21T14:23:49Z,0,sd,sd,v0.7.6,v0.7.6
2026-04-21T14:23:50Z,0,fzf,fzf,0.27.2,0.27.2
2026-04-21T14:23:50Z,0,bat,bat,v0.18.1,v0.18.1
2026-04-21T14:23:51Z,0,fcp,fcp,v0.1.0,v0.1.0
2026-04-21T14:23:52Z,1,ripgrep Windows,rg.exe,13.0.0,14.1.0
2026-04-21T14:23:53Z,0,dictomatic,dictomatic,First release,First release
...
$ ls -l | rg rg
.rwxr-xr-x  5.5M caleb  8 Feb  0:26  rg
.rwxrwxr-x  5.1M caleb  8 Feb  0:26  rg.exe
...
```

### Output format

`lifter` writes diagnostic logs to **stderr** (controlled with `-v`/`-vv`/`-q`)
and one CSV row per config section to **stdout**. The CSV has no header; the
columns are:

```
timestamp,was_updated,tool_name,file_name,previous_version,current_version
```

- `timestamp`: UTC RFC 3339, second precision (`YYYY-MM-DDTHH:MM:SSZ`).
- `was_updated`: `1` if a new artifact was downloaded this run, `0` otherwise.
- `tool_name`: the config section name.
- `file_name`: the file that was written (or would be, if it were updated).
- `previous_version`: what was recorded in `lifter.config` before this run.
- `current_version`: what was found on the remote this run (blank if the
  scrape found nothing).

This makes `lifter` trivially pipeable. To see only tools that were updated
this run:

```bash
$ ./lifter 2>/dev/null | awk -F, '$2==1'
2026-04-21T14:23:49Z,1,ripgrep,rg,13.0.0,14.1.0
2026-04-21T14:23:52Z,1,ripgrep Windows,rg.exe,13.0.0,14.1.0
```

Or to append updates to a changelog:

```bash
$ ./lifter 2>/dev/null \
    | awk -F, '$2==1 { printf("%s  %s: %s -> %s\n", $1, $3, $5, $6) }' \
    >> CHANGELOG
```

#### Logging doesn't get in the way

All `-v` / `-vv` / `-vvv` diagnostic output goes to **stderr**, so it never
contaminates the CSV on stdout. You can crank verbosity all the way up and
still pipe cleanly — stderr shows up on your terminal, stdout flows into
the next stage of the pipeline:

```bash
$ ./lifter -vv | awk -F, '$2==1'
INFO - [ripgrep] Downloading version 14.1.0
INFO - [ripgrep] Downloaded new version: 14.1.0
INFO - [ripgrep Windows] Downloading version 14.1.0
INFO - [ripgrep Windows] Downloaded new version: 14.1.0
2026-04-21T14:23:49Z,1,ripgrep,rg,13.0.0,14.1.0
2026-04-21T14:23:52Z,1,ripgrep Windows,rg.exe,13.0.0,14.1.0
```

(The `INFO` lines are stderr — your terminal interleaves them, but the
`awk` on the other side of the pipe only sees stdout.) Redirect stderr to
a file to separate cleanly: `./lifter -vv 2>lifter.log | awk ...`.

Unlike most package managers like *apt*, *scoop*, *brew*, *chocolatey*
and many others that focus on a single operating system, *lifter* can
download binaries for multiple operating
systems and simply place those in a directory. I regularly work on
computers with different operating systems and I like my tools to travel
with me. By merely copying (or syncing) my "binaries" directory, I have
everything available regardless of whether I'm on Linux or Windows.

This design only works because these applications can be deployed as
*single-file exectuables*. For more complex applications, a heavier
OS-specific package manager will be required.

## Usage

You just run the `lifter` binary, and it'll download the binaries.

There is a sample configuration file, `lifter.config` that lets you specify which
applications you want, and from where. *lifter* will keep track of the most recent
version, so it is cheap to rerun if nothing's changed.

This repo contains an example `lifter.config` file that you can use as a
starting point. It already contains sections for many popular golang and
rustlang single-file-executable programs, like
[ripgrep](https://github.com/BurntSushi/ripgrep),
[fzf](https://github.com/junegunn/fzf),
[starship](https://github.com/starship/starship),
and many others.

*lifter* works with other sites besides Github. The sample `lifter.config`
includes a definition for downloading the amazing _redbean_ binary
from @jart's site `https://justine.lol/redbean/`. You should check
out that project, it's wild.

### Automation

You can automate `lifter` using cron. Run `$ crontab -e` and then add:

```
SHELL=/bin/bash
24 22 * * * /path/to/lifter -w /path/for/downloads/
```

## Details

I said that *lifter* is for fetching CLI binaries. That's what I'm *using* it
for, but it's more than that. It's an engine for downloading things from
web pages. It works like a web scraper.  There is a declarative mechanism
for specifying how to find the download item on a page. You do have to
do a bit of work to figure out the right CSS to target the download
link correctly.

*NOTE: this section is out of date because of the switch from page
scraping to calling the Github API*

Let's look at the ripgrep configuration entry:

```ini
[ripgrep]
page_url = https://github.com/BurntSushi/ripgrep/releases/
anchor_tag = html main div.release-entry div.release-main-section details a
anchor_text = ripgrep-(\d+\.\d+\.\d+)-x86_64-unknown-linux-musl.tar.gz
version_tag = div.release-header a
target_filename_to_extract_from_archive = rg
version = 12.1.1

[ripgrep Windows]
page_url = https://github.com/BurntSushi/ripgrep/releases/
anchor_tag = html main div.release-entry div.release-main-section details a
anchor_text = ripgrep-(\d+\.\d+\.\d+)-x86_64-pc-windows-msvc.zip
version_tag = div.release-header a
target_filename_to_extract_from_archive = rg.exe
version = 12.1.1
```

Each section will download a file; one for Linux and one for Windows.
The `anchor_tag` is the CSS selector for finding a section that contains
the target download link.

If there are many tags matching the `anchor_tag`, all of them will be
checked to match the required `anchor_text`. This is how the Github
Releases page works. In one "release" section, there can be many file
downloads available. For example, one for each target architecture.
So the `anchor_tag`, alone, is not enough to target the specific target
file.

For that, we have the `anchor_text`: a regular expression that
will try to match the text of a specific `<a>` tag for all the `<a>` tags
in the items contained within the `anchor_tag` section. We're looking for
the specific `<a>` tag for the final download. In our *ripgrep* example,
the text regex contains placeholders for the version number (the `\d` values
for integers as part of the filename). These are discarded because we find
the version number in a different way, further below.

That's all that is needed to download the target file.

Two more details require explanation: tracking the version number,
and dealing with archives. For the version number, we have the `version_tag`,
which is also a CSS selector to find a DOM element containing the version
number to attach to the downloaded file. This version will also be **stored
and updated** in `lifter.config`. It is plausible that you might have a
situation with a (non-Github) target page where the version number does
not exist in its own DOM element. This scenario is currently unsupported.
I think I've come across it on a Sourceforge page, for example.

Finally, archives. Not all Github Releases artifacts are archives, some are
just the executables themselves. But in the ripgrep examples above, the Linux
download is a `.tar.gz` file, while the Windows download is a `.zip`.
By default, *lifter* will search within the archive to find a file that
matches the *name* of that section. So if a section is called `[sd]` then
*lifter* will search for a file called `sd` inside the `.tar.gz`
archive for that item. Likewise, for the section called `[sd.exe]`,
it'll look for `sd.exe` inside the zipfile for that section.

To override this, all you have to do is set the field
`target_filename_to_extract_from_archive`. If this is present, *lifter* will
use that name, rather than the name of the section to find the target file.
archive. For example, in our ripgrep examples, we called the
section name, say, `[ripgrep Windows]`, but the file that we intend
to extract from the archive is called `rg.exe`. This is why we
set the target filename for extraction, explicitly. For ripgrep,
we could remove the target filename setting if the section names were
changed to `[rg]` and `[rg.exe]`. In this case, the section names would
be the filenames lookup up in each respective archive.

Sometimes things aren't so neat and we'd prefer to rename whatever
is inside the archive. Consider the config for `[fcp]`:

```ini
[fcp]
template = github_release_latest
project = Svetlitski/fcp
target_filename_to_extract_from_archive = fcp-0.1.0-x86_64-unknown-linux-gnu
desired_filename = fcp
anchor_text = fcp-(\d+\.\d+\.\d+)-x86_64-unknown-linux-gnu.zip
version = v0.1.0
```

In this case, the name of the target executable as it appears inside the
release archive is `fcp-0.1.0-x86_64-unknown-linux-gnu`. We would
prefer that it be called `fcp` after extraction. To force this,
set the `desired_filename` field. The extracted executable will
be renamed to this after extraction.

## Templates

The description given in the *Details* section above is accurate but
laborious. It turns out that the CSS targeting is common for all
projects on the same site, e.g., Github Releases pages. Thus, there
is support for templates in the config file definition.

If you look at the example `lifter.config` file in this repo, what
you actually see for ripgrep is the following:

```ini
[template:github_release_latest]
page_url = https://github.com/{project}/releases
anchor_tag = html main details a
version_tag = div.release-header a

[ripgrep]
template = github_release_latest
project = BurntSushi/ripgrep
anchor_text = ripgrep-(\d+\.\d+\.\d+)-x86_64-unknown-linux-musl.tar.gz
target_filename_to_extract_from_archive = rg
version = 13.0.0

[starship.exe]
template = github_release_latest
project = starship/starship
anchor_text = starship-x86_64-pc-windows-msvc.zip
version = v0.55.0
```

What actually happens at runtime is that if a section, like `ripgrep`,
assigns a `template`, all the fields from that template are copied
into that section's declarations. In the example above, `page_url`,
`anchor_tag`, and `version_tag` will be copied into each of the
sections for `[ripgrep]` and `[starship.exe]`.

If you look carefully, you'll see that the template value for
`page_url` above contains the variable `{project}`. That will
be substituted for the value of `project` that is declared
inside each of the sections. In the above example, `page_url`
will be expanded to

```
page_url = https://github.com/BurntSushi/ripgrep/releases
```

for the `[ripgrep]` section, and expanded to

```
page_url = https://github.com/starship/starship/releases
```

for the `[starship.exe]` project.

## Github API

Github made a change to their _Releases_ pages that requires running
JavaScript to get the page to fully render. This change was likely
made to break scrapers like lifter. I have a working branch that uses
embedded Chrome to fully render pages (with JS) that works---but for
now I've implemented a method that uses the Github API to download
binaries, rather than scrape. I will monitor how smoothly this goes
and if it becomes too tedious I'll switch back from the API to
scraping with the embedded browser engine.

Using the API has both benefits and downsides. The only benefit for
lifter is that there might be more stability in the API than in the
_Releases_ HTML page structure. Scrapers usually suffer if websites
are updated frequently, in incompatible ways. There are several
downsides to using the API:
- There are more severe rate limits. This is particularly true for
unauthenticated requests, and for a tool like lifter which makes
a bunch of requests as its normal operation, is unusable, which means...
- You pretty much have to use authenticated requests, which means you
will need to provide a [Personal Access Token](https://github.com/settings/tokens)
- Tokens expire, which means you will have to periodically make a new
one and update your cron to use that. They _should_ expire because
tokens that never expire are a security risk. However, if a token
wasn't necessary you also wouldn't have the security risk. Needing
a token to get around the rate limits now also means you need to
manage token lifetime.
- Authentication means you can and will be tracked.

Because of these changes, the earlier description of how to configure
lifter will no longer work. However, the configuration is nearly the
same, except for two differences.

The first difference is in the config file, `lifter.config`. The
template section near the top must be written like this:

```inifile
[template:github_api_latest]
method = api_json
page_url = https://api.github.com/repos/{project}/releases/latest
version_tag = $.tag_name
anchor_tag = $.assets.*.browser_download_url
```

Note the change from `github_release_latest` to `github_api_latest`.
Then, simply change the `template` value only. Here's the example
for ripgrep:

```inifile
[ripgrep]
template = github_api_latest
project = BurntSushi/ripgrep
anchor_text = ripgrep-(\d+\.\d+\.\d+)-x86_64-unknown-linux-musl.tar.gz
target_filename_to_extract_from_archive = rg
version = 13.0.0
```

It is identical, except for the `template` value which now refers
to the new one.

The second change is that you must provide a personal access token
as an env var:

```bash
$ GITHUB_TOKEN=ghp_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx lifter -vv
```

It will run without specifying the token, but the rate limits come
very quickly, after only a handful of repos are checked.

## Geek creds

Lifter can update itself. The config entry required to allow lifter to
update itself looks like:

```ini
[lifter]
template = github_api_latest
project = cjrh/lifter
anchor_text = lifter-(\d+\.\d+\.\d+)-x86_64-unknown-linux-musl.tar.gz
version = 0.1.1

[lifter.exe]
template = github_api_latest
project = cjrh/lifter
anchor_text = lifter-(\d+\.\d+\.\d+)-x86_64-pc-windows-msvc.zip
version = 0.1.1
```

## Other alternatives

### Soar Package Manager

[Soar](https://github.com/pkgforge/soar) is a similar project that
provides a CLI interface for downloading binaries. It also automates
finding binaries. It is very extensive and is a much larger project
than *lifter*, and much more mature. You should probably use *soar*
instead of *lifter*.

From a design perspective, the primary difference between *lifter* 
and *soar* is that *soar* maintains its own archive of binaries, while *lifter* downloads
binaries directly from the github releases pages where the author
of the package has posted the binaries. Often this comes from
the CI of the project that the author has set up themselves.
There are pros and cons to each approach. With *lifter*, you only have to trust the authors
of the project you are downloading from, and I suppose you have to
trust me, the author of *lifter*, because you are probably not going
to read lifter source code. With *soar*, you have to trust the
maintainers of the *soar* project to make sure that the binaries
they are building are safe, since they rebuild all packages from source.
In my opinion this trust is reasonable to give and the *soar* project 
appears to be well-maintained. But overall, that's the difference.

### Huber

If all you want is to download binaries from Github, then
*Huber* is a probably a better choice than *lifter*.

[Huber](https://github.com/innobead/huber) is a similar project that
also uses the Github API to download binaries. It has a lot more
features than *lifter*, and is more mature.

Instead of a more general tool that is build around a "scraping"
mindset, *Huber* focuses specifically on Github releases via the Github
API. *Huber* makes it quite easy to list out various projects, and for
each project to list the available versions, based on what is available
on the Github Releases page for that project.

*Huber* has a "managed" list of projects that it can download. I have
been thinking about adding a similar feature to *lifter*, but I haven't
done it yet. For now you have to manage your `lifter.config` yourself.

Given that *Huber* exists, I'm going to focus lifter on being a more
general tool that can download from any site, not just Github.

### webinstall

A pre-existing project doing something similar is
[webinstall](https://github.com/webinstall/webi-installers). By comparison,
*lifter*:
- has fewer features
- has fewer options
- has fewer developers

*webinstall* is however more complex than *lifter*. *lifter* needs only 
itself (binary) and the `lifter.config` file to work.

## Releasing

Note to future-me: cutting a release is one command.

```bash
cargo install cargo-release  # one-time
cargo release patch --execute  # or `minor`, `major`, or a literal `X.Y.Z`
```

That invocation:

1. Bumps `version` in `Cargo.toml` and `Cargo.lock`.
2. Commits the bump.
3. Creates a git tag matching the new version, bare (e.g. `0.5.2`, no `v` prefix) — configured via `[package.metadata.release]` in `Cargo.toml`, which is what the release workflow's tag filter expects.
4. Pushes the commit and tag to `origin`.
5. Skips crates.io publish (not a library; `publish = false` in the same metadata block).

Drop `--execute` to see a dry run first.

### What CI does when the tag arrives

A push matching `[0-9]+.[0-9]+.[0-9]+` triggers `.github/workflows/release.yml`. It runs a single `build-release` matrix job across five targets:

| target                         | runner         | archive                                          |
| ------------------------------ | -------------- | ------------------------------------------------ |
| `x86_64-unknown-linux-musl`    | `ubuntu-22.04` | `lifter-X.Y.Z-x86_64-unknown-linux-musl.tar.gz`  |
| `arm-unknown-linux-gnueabihf`  | `ubuntu-22.04` | `lifter-X.Y.Z-arm-unknown-linux-gnueabihf.tar.gz`|
| `x86_64-apple-darwin`          | `macos-latest` | `lifter-X.Y.Z-x86_64-apple-darwin.tar.gz`        |
| `x86_64-pc-windows-msvc`       | `windows-2022` | `lifter-X.Y.Z-x86_64-pc-windows-msvc.zip`        |
| `i686-pc-windows-msvc`         | `windows-2022` | `lifter-X.Y.Z-i686-pc-windows-msvc.zip`          |

`cross` is only installed for the ARM target (pre-built binary, pinned via `CROSS_VERSION`); the other four targets build natively on their runners. Binaries are stripped automatically by `strip = "symbols"` in the release profile, so there's no explicit strip step.

Each matrix leg, on success, calls `softprops/action-gh-release` with the tag name. The first leg to finish creates the GitHub Release; subsequent legs attach their archive to the same release. `fail-fast: false` means if one target fails, the others still upload.

Action versions are SHA-pinned; dependabot (`.github/dependabot.yml`, `github-actions` ecosystem) bumps them on a daily schedule, so no manual maintenance is expected.

### If a release goes sideways

If a matrix leg fails and you want to retry cleanly:

```bash
git tag -d X.Y.Z                    # delete local tag
git push origin :refs/tags/X.Y.Z    # delete remote tag
gh release delete X.Y.Z             # if the release was already created
# fix the problem, then re-run `cargo release`
```

To re-run only the failed target without cutting a new tag, re-run the failed matrix leg from the GitHub Actions UI. If the leg failed *before* the upload step this just works; if it failed *during* upload and left a partial asset, delete that asset from the release page first (`softprops/action-gh-release` refuses to overwrite existing assets by default).
