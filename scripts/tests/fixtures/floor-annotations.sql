-- UPD1 re-check #5: the one annotation grammar. Every floor parser (the
-- migration check, update.mjs, runbook-server's build.rs) must read only
-- the exact form and agree on the floor; the check rejects each near miss.
-- contract(unused-since 0.3.0): the valid annotation
DROP TABLE a;
--contract(unused-since 9.9.1): no space after the dashes
  -- contract(unused-since 9.9.2): indented
-- Contract(Unused-Since 9.9.3): another case
-- contract(unused-since 9.9.4) : a space before the colon
-- contract( unused-since 9.9.5): a space inside the parenthesis
-- contract(unused-since 9.9.6):
-- contract(unused-since 9.9.7)  two spaces, no colon
-- contract(unused-since 0.2.0): another valid one, lower
DROP TABLE b;
