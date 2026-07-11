------------------------- MODULE WalReplication -------------------------
(* Primary/backup WAL shipping (topic 15's protocol, distilled).      *)
(* Entries are sequential 1..MaxLog and ship in order, so each log    *)
(* is a prefix of the primary's — one natural number per replica.     *)
(*                                                                    *)
(* SyncCommit = TRUE : primary commits entry k only once k is on a    *)
(*                     Quorum of replicas (itself included).          *)
(* SyncCommit = FALSE: primary commits as soon as the entry is local  *)
(*                     — the "removed ack". TLC finds the data-loss   *)
(*                     trace in seconds: Append, Commit, Crash,       *)
(*                     Failover to a replica that never saw entry 1.  *)

EXTENDS Integers, FiniteSets

CONSTANTS Replicas, MaxLog, Quorum, SyncCommit

VARIABLES
    primary,    \* current primary
    crashed,    \* set of crashed replicas (crashes are permanent here)
    wal,        \* [Replicas -> 0..MaxLog], length of each prefix log
    committed   \* client-visible commit point

vars == <<primary, crashed, wal, committed>>

Alive == Replicas \ crashed

TypeOK ==
    /\ primary \in Replicas
    /\ crashed \subseteq Replicas
    /\ wal \in [Replicas -> 0..MaxLog]
    /\ committed \in 0..MaxLog

Init ==
    /\ primary \in Replicas
    /\ crashed = {}
    /\ wal = [r \in Replicas |-> 0]
    /\ committed = 0

\* Client write: primary appends the next entry to its local WAL.
Append ==
    /\ primary \notin crashed
    /\ wal[primary] < MaxLog
    /\ wal' = [wal EXCEPT ![primary] = @ + 1]
    /\ UNCHANGED <<primary, crashed, committed>>

\* WAL shipping: backup r pulls the next entry it is missing.
Ship(r) ==
    /\ r # primary /\ r \notin crashed /\ primary \notin crashed
    /\ wal[r] < wal[primary]
    /\ wal' = [wal EXCEPT ![r] = @ + 1]
    /\ UNCHANGED <<primary, crashed, committed>>

\* Replicas whose log already contains entry k (an implicit ack).
AckedBy(k) == {r \in Replicas : wal[r] >= k}

\* Advance the commit point — gated on quorum only if SyncCommit.
Commit ==
    /\ primary \notin crashed
    /\ committed < wal[primary]
    /\ SyncCommit => Cardinality(AckedBy(committed + 1)) >= Quorum
    /\ committed' = committed + 1
    /\ UNCHANGED <<primary, crashed, wal>>

\* Crash anywhere, as long as a quorum of replicas survives.
Crash(r) ==
    /\ r \notin crashed
    /\ Cardinality(Alive \ {r}) >= Quorum
    /\ crashed' = crashed \cup {r}
    /\ UNCHANGED <<primary, wal, committed>>

\* Failover: the longest surviving log wins (quorum intersection
\* guarantees it holds every committed entry — IF commits waited
\* for quorum acks).
Failover(r) ==
    /\ primary \in crashed
    /\ r \notin crashed
    /\ \A s \in Alive : wal[s] <= wal[r]
    /\ primary' = r
    /\ UNCHANGED <<crashed, wal, committed>>

Next ==
    \/ Append
    \/ Commit
    \/ \E r \in Replicas : Ship(r) \/ Crash(r) \/ Failover(r)

Spec == Init /\ [][Next]_vars

\* THE invariant: a live primary's WAL contains every committed entry.
\* (Logs are prefixes, so "contains entry k" is just wal >= k.)
Durability == primary \notin crashed => committed <= wal[primary]

=============================================================================
