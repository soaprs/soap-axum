# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Typed path and query DTO decoding, typed required/optional/repeated header
  parsing, and JSON body decoding on `RouteRequest`.
- `TypedJsonRouteIo` and `JsonResponse<T>` for request-to-input and
  output-to-response mapping without serialization logic in handlers or use
  cases.
- Safe adapter-owned `HttpRejection` mapping for malformed requests (400),
  unacceptable JSON responses (406), oversized bodies (413), and unsupported
  JSON request media types (415).
- Axum harness coverage for the shared `soaprs-contract-tests` HTTP adapter
  contract, including route registration, typed normalization and RouteIO,
  use-case isolation, response effects, and extension lifecycle ordering.
- Fail-closed runtime capability coverage for declared authentication,
  validation, rate-limit, CORS, and CSRF policies, with an explicit
  `allow_unenforced` escape hatch for externally enforced metadata.
- Deterministic router-level plugin phases: augmentations add preflight routes
  before wrappers apply outer policy/telemetry layers to every route, plus
  lifecycle observation of request-normalization rejections.
- Strict query percent-encoding and UTF-8 validation and RFC cookie-octet
  parsing, including quoted request-cookie values.
- A runnable `security_telemetry` example with application-owned CORS preflight,
  CSRF enforcement, outer response observation, typed RouteIO, and a pure use
  case.

### Changed

- `JsonRouteIo` now enforces JSON request content types and distinguishes
  malformed JSON syntax from structurally valid validation failures.

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
