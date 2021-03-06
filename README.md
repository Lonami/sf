# sf

Send Files in LAN quickly.

## Installation

```sh
cargo install --git https://github.com/lonami/sf
```

…or build and move the artifact somewhere in your `PATH`.

## Usage

Receive files:

```sh
sf
```

Send files:

```sh
sf <IP> [FILES...]
```

## Security considerations

There is no encryption and no checks to the file paths are made. The tool should only be used in LAN you control to quickly move files around computers.

Beware the paths are received in the same way they were sent, i.e. sending absolute paths will cause the receiver to receive absolute paths too, similar with relative paths going to parent directories (try to avoid it if possible).

## License

The program is licensed under either of

* Apache License, Version 2.0 ([LICENSE-APACHE] or
  http://www.apache.org/licenses/LICENSE-2.0)

* MIT license ([LICENSE-MIT] or http://opensource.org/licenses/MIT)

at your option.

[LICENSE-APACHE]: https://github.com/Lonami/sf/blob/master/LICENSE-APACHE
[LICENSE-MIT]: https://github.com/Lonami/sf/blob/master/LICENSE-MIT
