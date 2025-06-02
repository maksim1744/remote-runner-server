# remote-runner-server

Execute arbitrary commands on your remote server via http.
Mainly created to be used with `cpr approx` over ssh port forwarding, refer to [cpr-approx-demo](https://github.com/maksim1744/cpr-approx-demo/blob/master/README.md) for more info.

### Remote installation

- Install [rust](https://www.rust-lang.org/tools/install)
- Install the server
    ```sh
    cargo install --git https://github.com/maksim1744/remote-runner-server
    ```
- Start the server
    ```sh
    screen -m -d bash -c "RUST_LOG=info remote-runner-server --port 1234"
    ```

### Local installation

- None, just forward the port:
    ```sh
    ssh -L 5678:localhost:1234 user@host  # optionally add -N before -L to hide the shell
    ```
- And test that it works
    ```sh
    curl "http://localhost:5678/ping"
    ```
