//! Integration tests for control-flow recovery and block coverage.

#[cfg(test)]
mod recovery_tests {
    use abyssflower_lib::cfg::{builder as cfg_builder, DomTree};
    use abyssflower_lib::ir::{recover, recover_with_branch_convergence, Stmt};
    use abyssflower_lib::ClassFile;

    fn load(path: &str) -> Vec<u8> {
        std::fs::read(path).unwrap_or_else(|_| panic!("cannot read {}", path))
    }

    fn base(name: &str) -> String {
        format!("vineflower-master/testData/classes/custom/{}", name)
    }

    /// Run recovery on every method in the given bytes, return (class_name, errors).
    fn recover_all_methods(bytes: &[u8]) -> (String, Vec<String>) {
        let cf = ClassFile::parse(bytes).expect("parse failed");
        let mut errors = Vec::new();

        for method in &cf.methods {
            let code = match method.code() {
                Some(c) => c,
                None => continue,
            };
            let cfg = cfg_builder::build(code);
            let dom = DomTree::compute(&cfg);

            // Recovery must not panic
            let (arena, root) = recover(&cfg, &dom, code);

            // Sanity: root stmt was produced
            let _ = arena.get(root);

            // Count blocks covered by the stmt tree
            let covered = count_covered_blocks(&arena, root, &arena);
            let total = cfg.real_blocks().count();

            if covered == 0 && total > 0 {
                errors.push(format!(
                    "{}.{}: 0 blocks covered out of {}",
                    cf.this_class, method.name, total
                ));
            }
        }

        (cf.this_class.clone(), errors)
    }

    /// Walk a stmt tree and count how many unique BlockIds are referenced.
    fn count_covered_blocks(
        arena: &abyssflower_lib::ir::StmtArena,
        id: abyssflower_lib::ir::StmtId,
        _a: &abyssflower_lib::ir::StmtArena,
    ) -> usize {
        match arena.get(id) {
            Stmt::Block(_) => 1,
            Stmt::Exit => 0,
            Stmt::Seq(s) => s
                .children
                .iter()
                .map(|&c| count_covered_blocks(arena, c, _a))
                .sum(),
            Stmt::If(s) => {
                1 + count_covered_blocks(arena, s.then_branch, _a)
                    + s.else_branch
                        .map(|e| count_covered_blocks(arena, e, _a))
                        .unwrap_or(0)
            }
            Stmt::Loop(s) => 1 + count_covered_blocks(arena, s.body, _a),
            Stmt::BreakIf(_) => 1,
            Stmt::Switch(s) => {
                1 + s
                    .arms
                    .iter()
                    .map(|a| count_covered_blocks(arena, a.body, _a))
                    .sum::<usize>()
            }
            Stmt::TryCatch(s) => {
                count_covered_blocks(arena, s.try_body, _a)
                    + s.catches
                        .iter()
                        .map(|c| count_covered_blocks(arena, c.body, _a))
                        .sum::<usize>()
                    + s.finally_body
                        .map(|f| count_covered_blocks(arena, f, _a))
                        .unwrap_or(0)
            }
            Stmt::Synchronized(s) => 1 + count_covered_blocks(arena, s.body, _a),
        }
    }

    // ── smoke tests: recovery must not panic ──────────────────────────

    macro_rules! smoke_test {
        ($name:ident, $file:expr) => {
            #[test]
            #[ignore = "requires vineflower testData"]
            fn $name() {
                let bytes = load(&base($file));
                let (class, errors) = recover_all_methods(&bytes);
                assert!(
                    errors.is_empty(),
                    "Recovery errors in {}:\n  {}",
                    class,
                    errors.join("\n  ")
                );
            }
        };
        ($name:ident, full: $file:expr) => {
            #[test]
            #[ignore = "requires vineflower testData"]
            fn $name() {
                let bytes = load($file);
                let (class, errors) = recover_all_methods(&bytes);
                assert!(
                    errors.is_empty(),
                    "Recovery errors in {}:\n  {}",
                    class,
                    errors.join("\n  ")
                );
            }
        };
    }

    #[test]
    fn recover_repository_fixture() {
        let bytes = include_bytes!("java_classes/fixture/ControlFlowFixture.class");
        let (class, errors) = recover_all_methods(bytes);
        assert!(
            errors.is_empty(),
            "Recovery errors in {}:\n  {}",
            class,
            errors.join("\n  ")
        );
    }

    smoke_test!(recover_main, "../bulk/pkg/Main.class");
    smoke_test!(recover_switch_enum, "TestEclipseSwitchEnum.class");
    smoke_test!(recover_switch_string, "TestEclipseSwitchString.class");
    smoke_test!(recover_jsr, "TestJsr.class");
    smoke_test!(recover_jsr2, "TestJsr2.class");
    smoke_test!(recover_hotjava, "TestHotjava.class");
    smoke_test!(recover_synchronized, "TestJava1Synchronized.class");
    smoke_test!(recover_string_concat, "TestStringConcatJ19.class");
    smoke_test!(recover_switch_j21, "TestSwitchOnEnumWithoutEnumJ21.class");
    smoke_test!(recover_inner_ctor,     full: "vineflower-master/testData/classes/custom/v11/TestInnerClassConstructor.class");

    // ── structural shape tests ──────────────────────────────────────────

    /// A straight-line method `() -> void { return; }` should give a
    /// single Block stmt wrapping the return.
    #[test]
    fn test_recover_trivial_block() {
        use abyssflower_lib::classfile::attribute::CodeAttribute;
        use abyssflower_lib::classfile::instruction::{InsnKind, Instruction};

        let code = CodeAttribute {
            max_stack: 0,
            max_locals: 1,
            instructions: vec![
                Instruction {
                    offset: 0,
                    opcode: 0xb1,
                    wide: false,
                    kind: InsnKind::NoOperand,
                }, // return
            ],
            exception_table: vec![],
            attributes: vec![],
        };
        let cfg = cfg_builder::build(&code);
        let dom = DomTree::compute(&cfg);
        let (arena, root) = recover(&cfg, &dom, &code);

        // Root should be a single Block
        assert!(
            matches!(arena.get(root), Stmt::Block(_) | Stmt::Seq(_)),
            "Expected Block or Seq for trivial method, got {:?}",
            arena.get(root)
        );
    }

    /// A conditional branch:
    ///   0: ifeq +5
    ///   3: return
    ///   5: return
    /// Should produce an If statement.
    #[test]
    fn test_recover_simple_if() {
        use abyssflower_lib::classfile::attribute::CodeAttribute;
        use abyssflower_lib::classfile::instruction::{InsnKind, Instruction};

        let code = CodeAttribute {
            max_stack: 1,
            max_locals: 1,
            instructions: vec![
                Instruction {
                    offset: 0,
                    opcode: 0x99, /*ifeq*/
                    wide: false,
                    kind: InsnKind::Branch { offset: 5 },
                },
                Instruction {
                    offset: 3,
                    opcode: 0xb1, /*return*/
                    wide: false,
                    kind: InsnKind::NoOperand,
                },
                Instruction {
                    offset: 5,
                    opcode: 0xb1, /*return*/
                    wide: false,
                    kind: InsnKind::NoOperand,
                },
            ],
            exception_table: vec![],
            attributes: vec![],
        };
        let cfg = cfg_builder::build(&code);
        let dom = DomTree::compute(&cfg);
        let (arena, root) = recover(&cfg, &dom, &code);

        // Walk and find at least one If node
        fn has_if(arena: &abyssflower_lib::ir::StmtArena, id: abyssflower_lib::ir::StmtId) -> bool {
            match arena.get(id) {
                Stmt::If(_) => true,
                Stmt::Seq(s) => s.children.iter().any(|&c| has_if(arena, c)),
                Stmt::Loop(s) => has_if(arena, s.body),
                _ => false,
            }
        }

        assert!(
            has_if(&arena, root),
            "Expected an If statement in recovery of conditional branch"
        );
    }

    #[test]
    fn early_return_guard_does_not_duplicate_shared_continuation() {
        use abyssflower_lib::classfile::attribute::CodeAttribute;
        use abyssflower_lib::classfile::instruction::{InsnKind, Instruction};

        let instruction = |offset, opcode, kind| Instruction {
            offset,
            opcode,
            wide: false,
            kind,
        };
        // The outer branch can join at pc 9, while its fall-through contains
        // an inner early return at pc 12.  EXIT is the formal post-dominator,
        // but pc 9 is the source-level continuation of the outer guard.
        let code = CodeAttribute {
            max_stack: 1,
            max_locals: 2,
            instructions: vec![
                instruction(0, 0x1a, InsnKind::NoOperand),
                instruction(1, 0x99, InsnKind::Branch { offset: 8 }),
                instruction(4, 0x1b, InsnKind::NoOperand),
                instruction(5, 0x99, InsnKind::Branch { offset: 7 }),
                instruction(8, 0x00, InsnKind::NoOperand),
                instruction(9, 0x00, InsnKind::NoOperand),
                instruction(10, 0xb1, InsnKind::NoOperand),
                instruction(12, 0xb1, InsnKind::NoOperand),
            ],
            exception_table: vec![],
            attributes: vec![],
        };
        let cfg = cfg_builder::build(&code);
        let shared = cfg
            .real_blocks()
            .find(|block| block.start_offset == 9)
            .unwrap()
            .id;
        let outer = cfg
            .real_blocks()
            .find(|block| block.start_offset == 0)
            .unwrap()
            .id;
        let dom = DomTree::compute(&cfg);
        let (arena, root) = recover_with_branch_convergence(&cfg, &dom, &code);

        fn inspect(
            arena: &abyssflower_lib::ir::StmtArena,
            id: abyssflower_lib::ir::StmtId,
            outer: u32,
            shared: u32,
        ) -> (usize, Option<u32>) {
            match arena.get(id) {
                Stmt::Block(block) => (usize::from(block.block_id == shared), None),
                Stmt::Seq(sequence) => {
                    sequence
                        .children
                        .iter()
                        .fold((0, None), |(count, post), child| {
                            let (child_count, child_post) = inspect(arena, *child, outer, shared);
                            (count + child_count, post.or(child_post))
                        })
                }
                Stmt::If(branch) => {
                    let (then_count, then_post) = inspect(arena, branch.then_branch, outer, shared);
                    let (else_count, else_post) = branch
                        .else_branch
                        .map(|branch| inspect(arena, branch, outer, shared))
                        .unwrap_or((0, None));
                    (
                        then_count + else_count,
                        (branch.cond_block == outer)
                            .then_some(branch.post_block)
                            .flatten()
                            .or(then_post)
                            .or(else_post),
                    )
                }
                Stmt::Loop(loop_stmt) => inspect(arena, loop_stmt.body, outer, shared),
                Stmt::Switch(switch) => switch.arms.iter().fold((0, None), |(count, post), arm| {
                    let (arm_count, arm_post) = inspect(arena, arm.body, outer, shared);
                    (count + arm_count, post.or(arm_post))
                }),
                Stmt::TryCatch(try_catch) => {
                    let mut result = inspect(arena, try_catch.try_body, outer, shared);
                    for catch in &try_catch.catches {
                        let caught = inspect(arena, catch.body, outer, shared);
                        result.0 += caught.0;
                        result.1 = result.1.or(caught.1);
                    }
                    result
                }
                Stmt::Synchronized(sync) => inspect(arena, sync.body, outer, shared),
                Stmt::BreakIf(_) | Stmt::Exit => (0, None),
            }
        }

        let (shared_occurrences, outer_post) = inspect(&arena, root, outer, shared);
        assert_eq!(outer_post, Some(shared));
        assert_eq!(shared_occurrences, 1);
    }

    /// A simple loop:
    ///   0: iload_0
    ///   1: ifeq  +5  (exit loop if == 0)
    ///   4: goto   0  (back edge)
    ///   7: return
    #[test]
    fn test_recover_simple_loop() {
        use abyssflower_lib::classfile::attribute::CodeAttribute;
        use abyssflower_lib::classfile::instruction::{InsnKind, Instruction};

        let code = CodeAttribute {
            max_stack: 1,
            max_locals: 1,
            instructions: vec![
                // offset 0: iload_0  (1 byte)
                Instruction {
                    offset: 0,
                    opcode: 0x1a, /*iload_0*/
                    wide: false,
                    kind: InsnKind::NoOperand,
                },
                // offset 1: ifeq +6 → target 7 (exit)  (3 bytes)
                Instruction {
                    offset: 1,
                    opcode: 0x99, /*ifeq*/
                    wide: false,
                    kind: InsnKind::Branch { offset: 6 },
                },
                // offset 4: goto -4 → target 0 (back-edge)  (3 bytes)
                Instruction {
                    offset: 4,
                    opcode: 0xa7, /*goto*/
                    wide: false,
                    kind: InsnKind::Branch { offset: -4 },
                },
                // offset 7: return  (1 byte)
                Instruction {
                    offset: 7,
                    opcode: 0xb1, /*return*/
                    wide: false,
                    kind: InsnKind::NoOperand,
                },
            ],
            exception_table: vec![],
            attributes: vec![],
        };
        let cfg = cfg_builder::build(&code);
        let dom = DomTree::compute(&cfg);
        let (arena, root) = recover(&cfg, &dom, &code);

        fn has_loop(
            arena: &abyssflower_lib::ir::StmtArena,
            id: abyssflower_lib::ir::StmtId,
        ) -> bool {
            match arena.get(id) {
                Stmt::Loop(_) => true,
                Stmt::Seq(s) => s.children.iter().any(|&c| has_loop(arena, c)),
                _ => false,
            }
        }

        assert!(
            has_loop(&arena, root),
            "Expected a Loop statement in recovery of simple back-edge loop"
        );
    }

    #[test]
    fn test_shared_catch_continuation_stays_outside_last_handler() {
        use abyssflower_lib::classfile::attribute::{CodeAttribute, ExceptionHandler};
        use abyssflower_lib::classfile::instruction::{InsnKind, Instruction};

        let instruction = |offset, opcode, kind| Instruction {
            offset,
            opcode,
            wide: false,
            kind,
        };
        let code = CodeAttribute {
            max_stack: 1,
            max_locals: 2,
            instructions: vec![
                instruction(0, 0x00, InsnKind::NoOperand),
                instruction(1, 0xa7, InsnKind::Branch { offset: 8 }),
                instruction(4, 0x4c, InsnKind::NoOperand),
                instruction(5, 0xa7, InsnKind::Branch { offset: 4 }),
                instruction(8, 0x4c, InsnKind::NoOperand),
                instruction(9, 0xb1, InsnKind::NoOperand),
            ],
            exception_table: vec![
                ExceptionHandler {
                    start_pc: 0,
                    end_pc: 1,
                    handler_pc: 4,
                    catch_type: Some("java/lang/IllegalArgumentException".into()),
                },
                ExceptionHandler {
                    start_pc: 0,
                    end_pc: 1,
                    handler_pc: 8,
                    catch_type: Some("java/lang/Exception".into()),
                },
            ],
            attributes: vec![],
        };
        let cfg = cfg_builder::build(&code);
        let dom = DomTree::compute(&cfg);
        let (arena, root) = recover(&cfg, &dom, &code);
        let continuation = cfg
            .real_blocks()
            .find(|block| block.start_offset == 9)
            .unwrap()
            .id;

        fn contains_block(
            arena: &abyssflower_lib::ir::StmtArena,
            id: abyssflower_lib::ir::StmtId,
            expected: u32,
        ) -> bool {
            match arena.get(id) {
                Stmt::Block(block) => block.block_id == expected,
                Stmt::Seq(seq) => seq
                    .children
                    .iter()
                    .any(|child| contains_block(arena, *child, expected)),
                Stmt::If(stmt) => {
                    contains_block(arena, stmt.then_branch, expected)
                        || stmt
                            .else_branch
                            .is_some_and(|branch| contains_block(arena, branch, expected))
                }
                Stmt::Loop(stmt) => contains_block(arena, stmt.body, expected),
                Stmt::Switch(stmt) => stmt
                    .arms
                    .iter()
                    .any(|arm| contains_block(arena, arm.body, expected)),
                Stmt::TryCatch(stmt) => {
                    contains_block(arena, stmt.try_body, expected)
                        || stmt
                            .catches
                            .iter()
                            .any(|catch| contains_block(arena, catch.body, expected))
                        || stmt
                            .finally_body
                            .is_some_and(|body| contains_block(arena, body, expected))
                }
                Stmt::Synchronized(stmt) => contains_block(arena, stmt.body, expected),
                Stmt::BreakIf(_) => false,
                Stmt::Exit => false,
            }
        }

        let Stmt::Seq(root_seq) = arena.get(root) else {
            panic!("expected try/catch followed by its continuation");
        };
        let try_stmt = root_seq
            .children
            .iter()
            .find_map(|id| match arena.get(*id) {
                Stmt::TryCatch(stmt) => Some(stmt),
                _ => None,
            })
            .expect("try/catch not recovered");
        assert!(try_stmt.catches.iter().all(|catch| !contains_block(
            &arena,
            catch.body,
            continuation
        )));
        assert!(root_seq
            .children
            .iter()
            .any(|id| contains_block(&arena, *id, continuation)));
    }

    #[test]
    fn recovers_conditional_latch_exit_as_break() {
        let bytes = std::fs::read("tests/java_classes/fixture/WriterEdgeFixture.class").unwrap();
        let class = abyssflower_lib::ClassFile::parse(&bytes).unwrap();
        let method = class
            .methods
            .iter()
            .find(|method| method.name == "latchGuardLoop")
            .unwrap();
        let code = method.code().unwrap();
        let cfg = cfg_builder::build(code);
        let dom = DomTree::compute(&cfg);
        let (arena, root) = recover(&cfg, &dom, code);

        fn contains_break_if(
            arena: &abyssflower_lib::ir::StmtArena,
            id: abyssflower_lib::ir::StmtId,
        ) -> bool {
            match arena.get(id) {
                Stmt::BreakIf(_) => true,
                Stmt::Seq(sequence) => sequence
                    .children
                    .iter()
                    .any(|child| contains_break_if(arena, *child)),
                Stmt::If(branch) => {
                    contains_break_if(arena, branch.then_branch)
                        || branch
                            .else_branch
                            .is_some_and(|branch| contains_break_if(arena, branch))
                }
                Stmt::Loop(loop_stmt) => contains_break_if(arena, loop_stmt.body),
                Stmt::Switch(switch) => switch
                    .arms
                    .iter()
                    .any(|arm| contains_break_if(arena, arm.body)),
                Stmt::TryCatch(try_catch) => {
                    contains_break_if(arena, try_catch.try_body)
                        || try_catch
                            .catches
                            .iter()
                            .any(|catch| contains_break_if(arena, catch.body))
                }
                Stmt::Synchronized(sync) => contains_break_if(arena, sync.body),
                Stmt::Block(_) | Stmt::Exit => false,
            }
        }

        assert!(contains_break_if(&arena, root));
    }
}
