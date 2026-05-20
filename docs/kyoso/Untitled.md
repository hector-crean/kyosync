


Querying out scene graph
Have to query the actual bevy world, as it requires total context (i.e. not just replicated types)

RPC query method?


presumably we can do a DFS? Presumably the tree is also instatiated on the bevy side, and not just within the CRDT tree?

Persistent IDs? Each local client has its own mappign between the local, non peristent ids, and the persistent ids.
Presumabkly each entity could have a GlobaIId component?


