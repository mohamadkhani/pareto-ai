# Pareto AI

A self-hosted gateway that routes OpenAI-compatible chat requests to the
cheapest healthy provider for the requested model. The language below is the
vocabulary the operator, the API and the admin console share.

## Language

**Provider**:
A company whose HTTP API serves chat completions and whose account holds the
money, such as OpenRouter or SailResearch.
_Avoid_: Upstream, vendor, backend

**Offering**:
One model sold by one provider at one price, reachable through one base URL.
The unit the router chooses between.
_Avoid_: Model, endpoint, route

**Credential**:
One provider account's secret together with the headers needed to use it.
Several credentials may belong to the same provider.
_Avoid_: Key, account, token

**Request**:
One call a client makes to the gateway's `/v1` surface, identified by a request
id.
_Avoid_: Call, invocation

**Attempt**:
One provider call the gateway makes while serving a request. A request has one
or more attempts when it fails over or rotates credentials.
_Avoid_: Try, hit, retry

**Failover**:
Serving the same request with a different offering or credential after an
attempt failed, before the client has received a response body.
_Avoid_: Retry, fallback

**Upstream error**:
The failure an attempt ends with: the provider's non-success HTTP response and
its error body, an error event inside a streaming response, or a transport or
timeout failure with no provider response.
_Avoid_: Provider error, API error, upstream failure

**Error class**:
The gateway's own fixed category for a failed attempt or request, used for
routing, accounting and reporting. It never contains provider text.
_Avoid_: Error type, error code

**Error detail**:
The bounded, redacted excerpt of an upstream error kept for the operator: the
provider's message, its own code and type, its request id, and the HTTP status.
_Avoid_: Error body, stack trace, log line

**Breaker**:
The gateway's guard for one offering and credential pair. It opens after
consecutive failed attempts and excludes the pair from routing for a cooldown,
then admits a probe.
_Avoid_: Circuit, fuse, health check

**Trip**:
One moment a breaker moves from closed to open. A trip is recorded with its
cause so the operator can explain an outage after the fact.
_Avoid_: Failure, incident, event
