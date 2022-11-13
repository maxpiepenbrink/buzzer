# syntax=docker/dockerfile:1
FROM rust:1.65.0 as builder

WORKDIR /usr/src/app

COPY . .
RUN cargo install --path .

FROM rust:slim-buster
COPY --from=builder /usr/local/cargo/bin/buzzer /usr/bin/buzzer

CMD ["buzzer"]
EXPOSE 7777
