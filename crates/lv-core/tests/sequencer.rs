//! Sequencer behaviour: what is accepted, what is refused with which code,
//! and that replaying the log rebuilds the same project.

use lv_core::ids::{AgentId, ChangeSetId, DocumentId, GrantId, NodeId, OpId, PersonId, ProjectId, Version};
use lv_core::op::{DocKind, NodeContent, Op, OpBody, Parent, Relation, Sequenced, TextEdit, Timestamp};
use lv_core::principal::{Grant, GrantScope, Principal};
use lv_core::refusal::RefusalCode;
use lv_core::sequencer::{MemoryLog, Sequencer, SubmitError};
use lv_core::state::ChangeSetStatus;
use lv_core::store::Store;
use serde_json::{Map, json};

struct World {
    seq: Sequencer,
    log: MemoryLog,
    person: Grant,
    agent: Grant,
    /// A second person, for concurrent edits.
    other: Grant,
    doc: DocumentId,
}

fn grant(holder: Principal, scope: GrantScope) -> Grant {
    Grant::new(holder, scope)
}

impl World {
    /// A project with one empty note.
    fn new() -> World {
        let person = PersonId::new();
        let mut world = World {
            seq: Sequencer::new(ProjectId::new()),
            log: MemoryLog::default(),
            person: grant(Principal::Person { person }, GrantScope::Owner),
            agent: grant(Principal::Agent { agent: AgentId::new(), on_behalf_of: person }, GrantScope::Delegated),
            other: grant(Principal::Person { person: PersonId::new() }, GrantScope::Owner),
            doc: DocumentId::new(),
        };
        for g in [&world.person, &world.agent, &world.other] {
            world.log.add_grant(g.clone());
        }
        let doc = world.doc;
        world.ok(world.by_person(OpBody::CreateDocument { doc, kind: DocKind::Note, title: "Note".into() }));
        world
    }

    fn op(&self, grant: &Grant, body: OpBody) -> Op {
        Op {
            id: OpId::new(),
            project: self.seq.project(),
            principal: grant.holder,
            grant: grant.id,
            base: self.seq.head(),
            change_set: None,
            created_at: Timestamp::now(),
            body,
        }
    }

    fn by_person(&self, body: OpBody) -> Op {
        self.op(&self.person, body)
    }

    fn by_agent(&self, cs: ChangeSetId, body: OpBody) -> Op {
        let mut op = self.op(&self.agent, body);
        op.change_set = Some(cs);
        op
    }

    fn submit(&mut self, batch: Vec<Op>) -> Result<Vec<Sequenced>, RefusalCode> {
        self.seq.submit(&mut self.log, batch).map_err(|e| match e {
            SubmitError::Refused(r) => r.code,
            SubmitError::Log(never) => match never {},
        })
    }

    fn ok(&mut self, op: Op) -> Sequenced {
        let mut out = self.submit(vec![op]).unwrap_or_else(|code| panic!("refused: {code:?}"));
        out.pop().unwrap()
    }

    fn refused(&mut self, op: Op) -> RefusalCode {
        let head = self.seq.head();
        let code = self.submit(vec![op]).expect_err("expected a refusal");
        assert_eq!(self.seq.head(), head, "a refusal must not advance the head");
        code
    }

    /// Inserts a paragraph at the document root after `after`.
    fn para(&mut self, after: Option<NodeId>) -> NodeId {
        let node = NodeId::new();
        let parent = Parent::DocumentRoot(self.doc);
        self.ok(self.by_person(insert(node, parent, after, "paragraph")));
        node
    }

    fn root(&self) -> Vec<NodeId> {
        self.seq.state().children(Parent::DocumentRoot(self.doc))
    }
}

fn insert(node: NodeId, parent: Parent, after: Option<NodeId>, node_type: &str) -> OpBody {
    OpBody::InsertNode { node, parent, after, content: NodeContent { node_type: node_type.into(), fields: Map::new() } }
}

fn replace(node: NodeId, value: &str) -> OpBody {
    OpBody::EditText { node, edit: TextEdit::Replace { value: value.into() } }
}

// ---- Ordering and structure ---------------------------------------------

#[test]
fn positions_are_contiguous_from_one() {
    let mut w = World::new();
    let a = w.para(None);
    let b = w.para(Some(a));
    assert_eq!(w.seq.head(), Version(3));
    assert_eq!(w.log.ops.iter().map(|s| s.seq.0).collect::<Vec<_>>(), [1, 2, 3]);
    assert_eq!(w.root(), [a, b]);
}

#[test]
fn later_insert_after_same_anchor_comes_first() {
    let mut w = World::new();
    let a = w.para(None);
    let b = w.para(Some(a));
    let c = w.para(Some(a));
    assert_eq!(w.root(), [a, c, b]);
}

#[test]
fn deleted_anchor_falls_back_to_its_position() {
    let mut w = World::new();
    let a = w.para(None);
    let b = w.para(Some(a));
    let c = w.para(Some(b));
    w.ok(w.by_person(OpBody::DeleteNode { node: b }));
    let d = w.para(Some(b));
    // Lands where b was: after a, its nearest surviving left sibling.
    assert_eq!(w.root(), [a, d, c]);
}

#[test]
fn anchor_moved_away_means_first() {
    let mut w = World::new();
    let a = w.para(None);
    let b = w.para(Some(a));
    let list = NodeId::new();
    w.ok(w.by_person(insert(list, Parent::DocumentRoot(w.doc), Some(b), "list")));
    w.ok(w.by_person(OpBody::MoveNode { node: a, parent: Parent::Node(list), after: None }));
    let c = w.para(Some(a));
    assert_eq!(w.root(), [c, b, list]);
    assert_eq!(w.seq.state().children(Parent::Node(list)), [a]);
}

#[test]
fn moves_refuse_cycles_and_carry_subtrees_across_documents() {
    let mut w = World::new();
    let list = w.para(None);
    let item = NodeId::new();
    w.ok(w.by_person(insert(item, Parent::Node(list), None, "listItem")));

    let code = w.refused(w.by_person(OpBody::MoveNode { node: list, parent: Parent::Node(item), after: None }));
    assert_eq!(code, RefusalCode::Cycle);
    let code = w.refused(w.by_person(OpBody::MoveNode { node: list, parent: Parent::Node(list), after: None }));
    assert_eq!(code, RefusalCode::Cycle);

    let other = DocumentId::new();
    w.ok(w.by_person(OpBody::CreateDocument { doc: other, kind: DocKind::Draft, title: "Draft".into() }));
    w.ok(w.by_person(OpBody::MoveNode { node: list, parent: Parent::DocumentRoot(other), after: None }));
    assert!(w.root().is_empty());
    assert_eq!(w.seq.state().node(item).unwrap().document, other);
}

#[test]
fn deleted_nodes_refuse_edits_but_deleting_again_is_fine() {
    let mut w = World::new();
    let a = w.para(None);
    let child = NodeId::new();
    w.ok(w.by_person(insert(child, Parent::Node(a), None, "text")));
    w.ok(w.by_person(OpBody::DeleteNode { node: a }));
    w.ok(w.by_person(OpBody::DeleteNode { node: a }));

    assert!(w.seq.state().node(child).unwrap().deleted, "delete covers the subtree");
    assert_eq!(w.refused(w.by_person(replace(child, "x"))), RefusalCode::NodeDeleted);
    assert_eq!(w.refused(w.by_person(insert(NodeId::new(), Parent::Node(a), None, "text"))), RefusalCode::NodeDeleted);
}

#[test]
fn ops_in_deleted_documents_are_refused() {
    let mut w = World::new();
    let a = w.para(None);
    w.ok(w.by_person(OpBody::DeleteDocument { doc: w.doc }));
    assert_eq!(w.refused(w.by_person(replace(a, "x"))), RefusalCode::DocumentDeleted);
    assert_eq!(
        w.refused(w.by_person(OpBody::SetDocumentTitle { doc: w.doc, title: "t".into() })),
        RefusalCode::DocumentDeleted
    );
}

#[test]
fn malformed_ops_are_invalid() {
    let mut w = World::new();
    let a = w.para(None);
    let set = json!({ "type": "heading" }).as_object().unwrap().clone();
    assert_eq!(w.refused(w.by_person(OpBody::SetAttrs { node: a, set, unset: vec![] })), RefusalCode::InvalidOp);
    assert_eq!(
        w.refused(w.by_person(OpBody::SetRelation { node: a, relation: Some(Relation::new("supports")) })),
        RefusalCode::InvalidOp
    );
    assert_eq!(w.refused(w.by_person(insert(a, Parent::DocumentRoot(w.doc), None, "paragraph"))), RefusalCode::NodeExists);
    assert_eq!(w.refused(w.by_person(insert(NodeId::new(), Parent::DocumentRoot(w.doc), Some(NodeId::new()), "paragraph"))), RefusalCode::UnknownNode);
    assert_eq!(w.submit(vec![]), Err(RefusalCode::InvalidOp));
}

// ---- Text ---------------------------------------------------------------

#[test]
fn concurrent_text_replace_by_someone_else_is_stale() {
    let mut w = World::new();
    let a = w.para(None);
    let base = w.seq.head();

    w.ok(w.by_person(replace(a, "mine")));
    let mut theirs = w.op(&w.other, replace(a, "theirs"));
    theirs.base = base;
    assert_eq!(w.refused(theirs), RefusalCode::StaleText);

    // The same principal pipelining edits on an old base is fine.
    let mut again = w.by_person(replace(a, "mine, revised"));
    again.base = base;
    w.ok(again);
    assert_eq!(w.seq.state().node(a).unwrap().text(), Some("mine, revised"));

    // Someone who has seen the latest edit can replace it.
    w.ok(w.op(&w.other, replace(a, "theirs, rebased")));
    assert_eq!(w.seq.state().node(a).unwrap().text(), Some("theirs, rebased"));
}

// ---- Envelope checks ----------------------------------------------------

#[test]
fn envelope_checks() {
    let mut w = World::new();
    let a = w.para(None);

    let mut foreign = w.by_person(replace(a, "x"));
    foreign.project = ProjectId::new();
    assert_eq!(w.refused(foreign), RefusalCode::WrongProject);

    let mut future = w.by_person(replace(a, "x"));
    future.base = w.seq.head().next();
    assert_eq!(w.refused(future), RefusalCode::BaseInFuture);

    let replay = w.log.ops[1].op.clone();
    assert_eq!(w.refused(replay), RefusalCode::DuplicateOp);

    let twice = w.by_person(replace(a, "x"));
    assert_eq!(w.submit(vec![twice.clone(), twice]), Err(RefusalCode::DuplicateOp));
}

#[test]
fn permission_check_needs_a_matching_grant() {
    let mut w = World::new();
    let a = w.para(None);

    let mut unknown = w.by_person(replace(a, "x"));
    unknown.grant = GrantId::new();
    assert_eq!(w.refused(unknown), RefusalCode::Forbidden);

    // The agent may not use the person's grant, nor the other way round.
    let mut borrowed = w.op(&w.agent, replace(a, "x"));
    borrowed.grant = w.person.id;
    assert_eq!(w.refused(borrowed), RefusalCode::Forbidden);
}

#[test]
fn batches_are_all_or_nothing() {
    let mut w = World::new();
    let a = w.para(None);
    let head = w.seq.head();
    let logged = w.log.ops.len();
    let before = w.seq.state().clone();

    let good = w.by_person(replace(a, "applied?"));
    let bad = w.by_person(OpBody::DeleteNode { node: NodeId::new() });
    assert_eq!(w.submit(vec![good, bad]), Err(RefusalCode::UnknownNode));

    assert_eq!(w.seq.head(), head);
    assert_eq!(w.log.ops.len(), logged);
    assert_eq!(w.seq.state(), &before);

    // Later ops in a batch see earlier ones.
    let b = NodeId::new();
    let out = w
        .submit(vec![
            w.by_person(insert(b, Parent::DocumentRoot(w.doc), Some(a), "paragraph")),
            w.by_person(replace(b, "new")),
        ])
        .unwrap();
    assert_eq!(out.iter().map(|s| s.seq).collect::<Vec<_>>(), [head.next(), head.next().next()]);
}

// ---- Change sets --------------------------------------------------------

/// The agent proposes a paragraph containing a typed link.
fn propose(w: &mut World) -> (ChangeSetId, NodeId, NodeId) {
    let cs = ChangeSetId::new();
    let (para, link) = (NodeId::new(), NodeId::new());
    w.ok(w.by_agent(cs, OpBody::OpenChangeSet { change_set: cs, rationale: "Add the supporting claim".into() }));
    w.ok(w.by_agent(cs, insert(para, Parent::DocumentRoot(w.doc), None, "paragraph")));
    // Only valid on top of the proposal itself: `para` is not in the project.
    w.ok(w.by_agent(cs, insert(link, Parent::Node(para), None, "link")));
    w.ok(w.by_agent(cs, OpBody::SetRelation { node: link, relation: Some(Relation::new(Relation::SUPPORTS)) }));
    (cs, para, link)
}

#[test]
fn change_sets_apply_only_when_accepted() {
    let mut w = World::new();
    let (cs, para, link) = propose(&mut w);

    assert!(w.root().is_empty(), "pending proposal is not in the document");
    let view = w.seq.state().with_change_set(cs).unwrap();
    assert_eq!(view.children(Parent::DocumentRoot(w.doc)), [para]);

    w.ok(w.by_person(OpBody::AcceptChangeSet { change_set: cs }));
    assert_eq!(w.root(), [para]);
    let state = w.seq.state();
    assert_eq!(state.node(link).unwrap().relation, Some(Relation::new("supports")));
    assert_eq!(state.change_set(cs).unwrap().status, ChangeSetStatus::Accepted);
    assert_eq!(state.change_set(cs).unwrap().author, w.agent.holder);

    let late = w.by_agent(cs, OpBody::DeleteNode { node: para });
    assert_eq!(w.refused(late), RefusalCode::ChangeSetClosed);
}

#[test]
fn rejected_change_sets_never_apply() {
    let mut w = World::new();
    let (cs, _, _) = propose(&mut w);
    w.ok(w.by_person(OpBody::RejectChangeSet { change_set: cs, reason: Some("Not convincing".into()) }));
    assert!(w.root().is_empty());
    assert_eq!(w.refused(w.by_person(OpBody::AcceptChangeSet { change_set: cs })), RefusalCode::ChangeSetClosed);
}

#[test]
fn accepting_a_change_set_that_no_longer_applies_is_refused() {
    let mut w = World::new();
    let target = w.para(None);
    let cs = ChangeSetId::new();
    w.ok(w.by_agent(cs, OpBody::OpenChangeSet { change_set: cs, rationale: "Tighten".into() }));
    w.ok(w.by_agent(cs, replace(target, "tighter")));

    // Meanwhile the person deletes the paragraph the proposal edits.
    w.ok(w.by_person(OpBody::DeleteNode { node: target }));

    assert_eq!(w.refused(w.by_person(OpBody::AcceptChangeSet { change_set: cs })), RefusalCode::ChangeSetConflict);
    assert_eq!(w.seq.state().change_set(cs).unwrap().status, ChangeSetStatus::Open);
    // Adding to it now fails the same way.
    assert_eq!(w.refused(w.by_agent(cs, replace(target, "x"))), RefusalCode::ChangeSetConflict);
}

#[test]
fn change_set_rules() {
    let mut w = World::new();
    let cs = ChangeSetId::new();
    let a = w.para(None);
    assert_eq!(w.refused(w.by_agent(cs, replace(a, "x"))), RefusalCode::UnknownChangeSet);

    let outside = w.by_person(OpBody::OpenChangeSet { change_set: cs, rationale: "r".into() });
    assert_eq!(w.refused(outside), RefusalCode::InvalidOp);

    w.ok(w.by_agent(cs, OpBody::OpenChangeSet { change_set: cs, rationale: "r".into() }));
    let mut accept_inside = w.by_person(OpBody::AcceptChangeSet { change_set: cs });
    accept_inside.change_set = Some(cs);
    assert_eq!(w.refused(accept_inside), RefusalCode::InvalidOp);
    let reopen = w.by_agent(cs, OpBody::OpenChangeSet { change_set: cs, rationale: "r".into() });
    assert_eq!(w.refused(reopen), RefusalCode::ChangeSetExists);
}

// ---- Agent approval and auto-accept -------------------------------------

#[test]
fn agent_edits_must_be_in_a_change_set() {
    let mut w = World::new();
    let a = w.para(None);
    assert_eq!(w.refused(w.op(&w.agent, replace(a, "direct"))), RefusalCode::Forbidden);
    w.log.set_auto_accept(w.agent.id, true);
    assert_eq!(w.refused(w.op(&w.agent, replace(a, "direct"))), RefusalCode::Forbidden, "auto mode still uses change sets");
}

#[test]
fn agent_needs_approval_unless_auto_accept_is_on() {
    let mut w = World::new();
    let (cs, para, _) = propose(&mut w);

    assert_eq!(w.refused(w.op(&w.agent, OpBody::AcceptChangeSet { change_set: cs })), RefusalCode::Forbidden);

    assert!(w.log.set_auto_accept(w.agent.id, true));
    let accepted = w.ok(w.op(&w.agent, OpBody::AcceptChangeSet { change_set: cs }));
    assert_eq!(w.root(), [para]);

    // The log records that the agent approved its own change set, and when.
    assert_eq!(accepted.op.principal, w.agent.holder);
    assert_eq!(accepted.op.grant, w.agent.id);
    assert!(accepted.op.created_at.0 > 0);
    assert_eq!(w.seq.state().change_set(cs).unwrap().author, w.agent.holder);

    // Turning auto mode off again takes effect on the next op.
    w.log.set_auto_accept(w.agent.id, false);
    let (cs2, ..) = propose(&mut w);
    assert_eq!(w.refused(w.op(&w.agent, OpBody::AcceptChangeSet { change_set: cs2 })), RefusalCode::Forbidden);
}

#[test]
fn auto_accept_works_within_one_batch() {
    let mut w = World::new();
    w.log.set_auto_accept(w.agent.id, true);
    let cs = ChangeSetId::new();
    let para = NodeId::new();
    let batch = vec![
        w.by_agent(cs, OpBody::OpenChangeSet { change_set: cs, rationale: "Quick fix".into() }),
        w.by_agent(cs, insert(para, Parent::DocumentRoot(w.doc), None, "paragraph")),
        w.op(&w.agent, OpBody::AcceptChangeSet { change_set: cs }),
    ];
    w.submit(batch).unwrap();
    assert_eq!(w.root(), [para]);
}

#[test]
fn agents_decide_only_on_their_own_change_sets() {
    let mut w = World::new();
    let rival = Grant {
        auto_accept: true,
        ..grant(Principal::Agent { agent: AgentId::new(), on_behalf_of: w.person.holder.person() }, GrantScope::Delegated)
    };
    w.log.add_grant(rival.clone());
    w.log.set_auto_accept(w.agent.id, true);

    let (cs, ..) = propose(&mut w);
    assert_eq!(w.refused(w.op(&rival, OpBody::AcceptChangeSet { change_set: cs })), RefusalCode::Forbidden);
    let reject = OpBody::RejectChangeSet { change_set: cs, reason: None };
    assert_eq!(w.refused(w.op(&rival, reject)), RefusalCode::Forbidden);
}

#[test]
fn agent_may_withdraw_its_own_proposal_without_auto_accept() {
    let mut w = World::new();
    let (cs, ..) = propose(&mut w);
    w.ok(w.op(&w.agent, OpBody::RejectChangeSet { change_set: cs, reason: Some("Superseded".into()) }));
    assert_eq!(w.seq.state().change_set(cs).unwrap().status, ChangeSetStatus::Rejected);
}

#[test]
fn unknown_change_set_is_reported_as_such_to_agents() {
    let mut w = World::new();
    w.log.set_auto_accept(w.agent.id, true);
    let code = w.refused(w.op(&w.agent, OpBody::AcceptChangeSet { change_set: ChangeSetId::new() }));
    assert_eq!(code, RefusalCode::UnknownChangeSet);
}

// ---- Replay -------------------------------------------------------------

#[test]
fn replaying_the_log_rebuilds_the_same_state() {
    let mut w = World::new();
    let a = w.para(None);
    let b = w.para(Some(a));
    w.ok(w.by_person(replace(b, "text")));
    let (accepted, ..) = propose(&mut w);
    w.ok(w.by_person(OpBody::AcceptChangeSet { change_set: accepted }));
    propose(&mut w); // left open
    w.ok(w.by_person(OpBody::DeleteNode { node: a }));

    let rebuilt = Sequencer::replay(w.seq.project(), w.log.ops.clone()).unwrap();
    assert_eq!(rebuilt.head(), w.seq.head());
    assert_eq!(rebuilt.state(), w.seq.state());
}

#[test]
fn sequencer_writes_through_the_store_and_reloads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("p.lvn");
    let project = ProjectId::new();
    let person = PersonId::new();
    let owner = grant(Principal::Person { person }, GrantScope::Owner);
    let doc = DocumentId::new();

    let expected = {
        let mut store = Store::create(&path, project, "P").unwrap();
        store.add_person(person, "P").unwrap();
        store.add_grant(&owner).unwrap();
        let mut seq = Sequencer::new(project);
        let op = |base, body| Op {
            id: OpId::new(),
            project,
            principal: owner.holder,
            grant: owner.id,
            base,
            change_set: None,
            created_at: Timestamp::now(),
            body,
        };
        let node = NodeId::new();
        seq.submit(&mut store, vec![op(Version(0), OpBody::CreateDocument { doc, kind: DocKind::Note, title: "N".into() })])
            .unwrap();
        seq.submit(&mut store, vec![op(Version(1), insert(node, Parent::DocumentRoot(doc), None, "paragraph"))])
            .unwrap();
        // A refused op leaves the file untouched.
        assert!(seq.submit(&mut store, vec![op(Version(2), OpBody::DeleteNode { node: NodeId::new() })]).is_err());
        seq.state().clone()
    };

    let store = Store::open(&path).unwrap();
    assert_eq!(store.head().unwrap(), Version(2));
    let reloaded = Sequencer::replay(project, store.ops_since(Version::EMPTY).unwrap()).unwrap();
    assert_eq!(reloaded.state(), &expected);
}
