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

<svg viewBox="0 0 500 500" xmlns="http://www.w3.org/2000/svg">
  <defs></defs>
  <rect x="71.048" y="59.503" width="328.597" height="190.941" style="fill: rgb(216, 216, 216); stroke: rgb(0, 0, 0);"></rect>
  <text style="white-space: pre; fill: rgb(51, 51, 51); font-family: Arial, sans-serif; font-size: 24.9px;" x="93.25" y="97.691">Hey</text>
</svg>

> :warning: WARNING: This is an *alpha-quality hobby project*. I do use this
> tool myself, but I started this project mainly to learn rust. While I
> appreciate community input, I don't have much extra time to spend on this and
> I'll be unresponsive to issue reports. I will however happily merge PRs with
> improvements.

## Demo

Requires the presence of a `lifter.config` file alongside the binary. You can
use the example one in this repo.

```bash
$ ls -lh | rg lifter
.rwxrwxr-x  6.9M caleb  2 Apr 12:27  lifter
.rw-rw-r--   14k caleb  2 Apr 14:41  lifter.config
$ ./lifter -vv
INFO - [thesauromatic.exe] Found a match on versions tag: Alpha, includes bumpversion
INFO - [thesauromatic.exe] Found version is not newer: Alpha, includes bumpversion; Skipping.
INFO - [tokei] Found a match on versions tag: v12.1.2
INFO - [tokei] Found version is not newer: v12.1.2; Skipping.
INFO - [ncspot] Found a match on versions tag: v0.7.3
INFO - [ncspot] Found version is not newer: v0.7.3; Skipping.
INFO - [starship.exe] Found a match on versions tag: v0.55.0
INFO - [starship.exe] Found version is not newer: v0.55.0; Skipping.
INFO - [caddy] Found a match on versions tag: v2.4.3
INFO - [caddy] Found version is not newer: v2.4.3; Skipping.
INFO - [gitea] Found a match on versions tag: v1.14.3
INFO - [gitea] Found version is not newer: v1.14.3; Skipping.
INFO - [ripgrep] Found a match on versions tag: 13.0.0
INFO - [ripgrep] Found version is not newer: 13.0.0; Skipping.
INFO - [sd] Found a match on versions tag: v0.7.6
INFO - [sd] Found version is not newer: v0.7.6; Skipping.
INFO - [fzf] Found a match on versions tag: 0.27.2
INFO - [fzf] Found version is not newer: 0.27.2; Skipping.
INFO - [bat] Found a match on versions tag: v0.18.1
INFO - [bat] Found version is not newer: v0.18.1; Skipping.
INFO - [fcp] Found a match on versions tag: v0.1.0
INFO - [fcp] Found version is not newer: v0.1.0; Skipping.
INFO - [ripgrep Windows] Found a match on versions tag: 13.0.0
INFO - [ripgrep Windows] Found version is not newer: 13.0.0; Skipping.
INFO - [dictomatic] Found a match on versions tag: First release
INFO - [dictomatic] Found version is not newer: First release; Skipping.
...
$ ls -l | rg rg
.rwxr-xr-x  5.5M caleb  8 Feb  0:26  rg
.rwxrwxr-x  5.1M caleb  8 Feb  0:26  rg.exe
...
```

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

A pre-existing project doing something very similar is
[webinstall](https://github.com/webinstall/webi-installers). By comparison,
*lifter*:
- has fewer features
- has fewer options
- has fewer developers

*lifter* needs only itself (binary) and the `lifter.config` file to
work.
