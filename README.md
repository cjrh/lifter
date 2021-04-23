# lifter

*lifter* is a CLI tool for downloading single-file executables
from sites like Github that make them available.

```
$ dictomatic lifter
lifter  noun    a thief who steals goods that are in a store
lifter  noun    an athlete who lifts barbells
```

Take your pick.

> :warning: WARNING: This is an *alpha-quality hobby project*. I do use this
> tool myself, but I started this project mainly to learn rust. While I
> appreciate community input, I don't have much extra time to spend on this and
> I'll be unresponsive to issue reports. I will however happily merge PRs with
> improvements.

## Demo

```bash
$ ls -lh | rg lifter
.rwxrwxr-x  6.9M caleb  2 Apr 12:27  lifter
.rw-rw-r--   14k caleb  2 Apr 14:41  lifter.config
$ ./lifter -vv
INFO - Found a match on versions tag: v1.11.0
INFO - Found a match on versions tag: v0.13.0
INFO - Found a match on versions tag: 1,0,0
INFO - Found a match on versions tag: v0.7.5
INFO - Found a match on versions tag: v0.12
INFO - Found a match on versions tag: v8.2.1
INFO - Found a match on versions tag: v0.1.0
...
$ ls -l | rg rg
.rwxr-xr-x  5.5M caleb  8 Feb  0:26  rg
.rwxrwxr-x  5.1M caleb  8 Feb  0:26  rg.exe
...
```

Unlike most package managers like *apt*, *scoop*, *brew*, *chocolatey*
and many others, *lifter* can download binaries for multiple operating
systems and simply place those in a directory. I regularly work on
computers with different operating systems and I like my tools to travel
with me. By merely copying (or syncing) my "binaries" directory, I have
everything available regardless of whether I'm on Linux or Windows.

This design only works because these applications can be deployed as
single-file exectuables. For more complex applications, a heavier
OS-specific package manager will be required.

## Usage

There is a configuration file, `lifter.config` that lets you specify which
files you want, and from where. *lifter* will keep track of the most recent
version, so it is cheap to rerun if nothing's changed.

This repo contains an example `lifter.config` file that you can use as a
starting point. It already contains sections for many popular golang and
rustlang single-file-executable programs, like
[ripgrep](https://github.com/BurntSushi/ripgrep),
[fzf](https://github.com/junegunn/fzf),
[starship](https://github.com/starship/starship),
and many others.

## Details

I said that *lifter* is for fetch CLI binaries. That's what I'm *using* it
for, but it's more than that. It's an engine for downloading things from
web pages. There is a mechanism for specifying how to find the item on a
page.

Let's look at the ripgrep configuration entry:

```inifile
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

Within the `anchor_tag`, there can be many sections: this is how the Github
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
number to attach to the downloaded file. This version will also be stored
and updated in `lifter.config`. It is plausible that you might have a
situation with a (non-Github) target page where the version number does
not exist in its own DOM element. This scenario is currently unsupported.
I think I've come across it on a Sourceforge page, for example.

Finally, archives. Not all Github Releases artifacts are archives, some are
just the executables themselves. But in the ripgrep examples above, the Linux
download is a `.tar.gz` file, while the Windows download is a `.zip`. To deal
with this, all you have to do is set the field
`target_filename_to_extract_from_archive`. If this is present, *lifter* will
aggressively try to extract a file with the given name from the downloaded
archive. It will use the file extension (`.tar.gz` or `.zip`, or `.tgz` and a
handful of others) to figure out how to do the decompression. If successful,
the end result will be the target filename in the output directory; in this
case, `rg` for the Linux target and `rg.exe` for the Windows target.
