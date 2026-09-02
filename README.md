# lsd

[![SEGV 
LICENSE](https://img.shields.io/static/v1?label=SEGV%20LICENSE&message=1.1&labelColor=0060A8&color=ffffff)](https://xn--gckvb8fzb.com/segv/)

*L*ocal*S*end *D*ispatcher is a non-interactive command line client for
[LocalSend][ls]. It wraps the official `localsend` core crate and replaces the
interactive terminal UI with a proper CLI that takes everything from the command
line and exits once the operation is done.

[<img src="https://xn--gckvb8fzb.com/images/chatroom.png" width="275">](https://xn--gckvb8fzb.com/contact/)

[ls]: https://github.com/localsend/localsend

## Install

```sh
git submodule update --init
cargo install --path .
```

The package is called `localsend-dispatcher` because `lsd` was already taken on
crates.io, but it builds and installs a single binary named `lsd`. It isn't on
crates.io itself, because the LocalSend core it depends on has not been
published there, so the dependency can only be satisfied from the submodule.

`vendor/localsend` is the LocalSend repository as a git submodule, and
`Cargo.toml` depends on `packages/core` inside it by path. This was done in
order to avoid the frikkin' 900MB Flutter submodule for a crate whose source is
around 600KB. Since `git submodule update --init` is not recursive by itself the
checkout stays at approximately 18MB.

## Usage

### Discover

```sh
lsd [--json] --discover
```

`--discover` announces the LSD device, collects the LocalSend devices that
answer and prints them:

```
$ lsd --discover
D [1] Smart Raspberry (10.0.0.20)
D [2] Juicy Lemon (10.0.0.31)
```

### Send

```sh
lsd [--json] --send <DEVICE> <FILE>...
```

`--send` sends one or more files to a device and exits. The device can be an
alias, a fingerprint, an IP address or an `IP:PORT`.

**Note:** An address is contacted directly and skips discovery. Files cannot be
directories.

```
$ lsd --send "Smart Raspberry" report.pdf notes.txt
S Smart Raspberry: Sent 2 files (10.4 MB, took 3s)
```

Use `-` in place of a file to read a text message from stdin and send it as a
message rather than a file. Stdin has to be a pipe and can only appear once:

```
$ echo "Hello World" | lsd --send "Smart Raspberry" -
S Smart Raspberry: Message sent (took 0s)
```

If `-` is combined with actual files, then the input is sent as a `.txt` file.

### Receive

```sh
lsd [--json] --receive
```

`--receive` waits for a single incoming transfer, accepts it from whoever sends
it, saves the files and exits. A transfer that is a text message is output as
text on the CLI instead of saving it to a file. LSD waits for `--timeout`
seconds (by default 60 for `--receive`) before it gives up and exits. This way
you can launch LSD and forget about it, because you don't need to worry that
it's going to run indefinitely.

```
$ lsd --receive
R Waiting for a transfer (60s)
R Juicy Lemon: Received 2 files (4.1 MB, took 2s)
```

### JSON

The `--json` flag replaces the text with one JSON document on stdout, so a
script/program can parse it:

```sh
host=$(lsd --json --discover | jq -r '.devices[0].host')
lsd --json --send "$host" report.pdf | jq .ok
```

Every document carries an `ok` field. `lsd` exits 0 when everything worked, 1
when the operation failed, and 2 on a usage error.

### Other Options

`--alias`, `--port` and `--destination` set the device name, the HTTP port and
the directory received files are saved in.

`--multicast` sets the group used for discovery, which defaults to `224.0.0.167`
and only ever needs changing on a network that filters it.

`--timeout` sets the seconds `lsd` waits without anything happening, which by
default is 10 seconds for discovery, and 60 seconds for a transfer to arrive or
to advance.

**Note:** LSD also reads the `LOCALSEND_ALIAS`, `LOCALSEND_PORT`,
`LOCALSEND_DESTINATION` and `LOCALSEND_MULTICAST` environment variables.

## Configuration

`lsd` shares its configuration directory with the official `localsend-cli`
(`$XDG_CONFIG_HOME/localsend-cli` or `~/.config/localsend-cli`). It reads
`config.toml` for `alias`, `port`, `destination` and `multicast`, and it reads
(or generates) `identity.pem`, the certificate whose fingerprint identifies this
device.

Both tools appear to the network as the same device, so a peer paired with one
auto-accepts transfers from the other. They also bind the same port and cannot
run at the same time. `--port 0` takes a free port instead and works for all
three commands, as the port is carried in its announcement.
