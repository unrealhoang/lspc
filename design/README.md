# Design

## Components

![Components](http://www.plantuml.com/plantuml/proxy?src=https://raw.github.com/unrealhoang/master/design/component.puml)

Thick arrow heads are for function call, slim arrow heads are for enqueue messages.

* Main Loop: Single Global State of system, events are handled synchronously.
* LSPHandler: Store state of a LSP Server it's controlling. There can be multiple LSPHandler instances running.
* RpcClient: Handle I/O.
