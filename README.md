# rust-gitgrep

Sometimes I forget where I put an experimental code deep hidden in the repository history.
Git has nice command line utility to search through all the files in all the history, like so:

    git grep main $(git rev-list HEAD)

but there are 2 problems.

* It greps each commit individually, which takes so much time in large repository.
* It shows duplicate matches even if the matched file has no changes at all between commits,
  which clutters the output and obscures other files' hits.
* It requires Unix like shell, which is not supported on Windows.

Oops, there are 3.

So I made a command line utility to do exactly what I wanted, plus some exercise in Rust.

## Prerequisites

* Cargo 1.55.0


## Run

    cargo run --release -- <pattern> <repo>

Note that remote repos are not supported, as the same as `git grep` native command.
