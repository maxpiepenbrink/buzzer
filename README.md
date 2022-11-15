# Buzzer Overview

This is a simple all-in-one, self-contained, shared-state buzzer toolkit.

All it needs to do is run and have network I/O, and preferably with HTTPS termination.

`$ cargo run` is all you need, it's currently hard coded to listen on port 7777.

## This Ain't Pretty!
In it's current form, this repo is a super quick and dirty personal weekend hackathon with no unit tests and very little refactoring or cleanup in two languages I don't get to use very often. You'll note there are many crimes here, such as:
* One huge JS blob in the .html that is the clientside game state
* One huge rust main.rs that is the serverside game state.
* No tests!! None! No TDD! No nothing!

Needless to say if this demands any more complexity, it'll be worth it to do a big refactoring cleanup, get code coverage going, etc.

# Structure

This architecture stands on two important "keepin' it simple" legs:
1. Almost all events flow to/from the client and server effectively flow in 1 very static cycle.
2. All room state is fully public.

## Diagram

![diagram of the program architecture](docs/buzzer-diagrams.png)