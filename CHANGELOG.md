# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0] - 2026-08-15

### Added

- Initial Axum 0.8 adapter that registers a `soaprs-http` endpoint catalog and
  binds endpoint identities to typed handlers or `UseCase` implementations.
- `RouteIo` request/result mapping that keeps normalized HTTP data and response
  construction outside application use cases.
- Ordered global and endpoint-local middleware, observational lifecycle hooks,
  and build-time router plugins.
- Normalized path, query, headers, cookies, extensions, and buffered body data;
  safe `SoapError` mapping; portable `HttpResponseEffects`; response security
  and cache policy projection; body limits; and endpoint deadlines.
- Optional authentication, validation, and rate-limit bridges that delegate
  mechanisms, engines, algorithms, storage, and business policy to external
  capability ports.
- Integration tests and runnable examples, including a complete reference
  application proving 401, 422, 201, and 429 request paths.

[Unreleased]: https://github.com/soaprs/soap-axum/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/soaprs/soap-axum/releases/tag/v0.5.0
