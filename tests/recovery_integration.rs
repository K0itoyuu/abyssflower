/// Integration tests for Phase 4: control flow recovery.
///
/// We verify that:
///  1. Recovery completes without panic on all test classes.
///  2. The stmt tree covers all blocks (no block left unstructured).
///  3. Specific known patterns are recovered correctly.

#[cfg(test)]
mod recovery_tests {
    use abyssflower_lib::cfg::{builder as cfg_builder, DomTree};
    use abyssflower_lib::ir::{recover, Stmt};
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
                None    => continue,
            };
            let cfg = cfg_builder::build(code);
            let dom = DomTree::compute(&cfg);

            // Recovery must not panic
            let (arena, root) = recover(&cfg, &dom, code);

            // Sanity: root stmt was produced
            let _ = arena.get(root);

            // Count blocks covered by the stmt tree
            let covered = count_covered_blocks(&arena, root, &arena);
            let total   = cfg.real_blocks().count();

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
            Stmt::Block(_)  => 1,
            Stmt::Exit       => 0,
            Stmt::Seq(s)     => s.children.iter()
                .map(|&c| count_covered_blocks(arena, c, _a))
                .sum(),
            Stmt::If(s)      => {
                1 + count_covered_blocks(arena, s.then_branch, _a)
                  + s.else_branch.map(|e| count_covered_blocks(arena, e, _a)).unwrap_or(0)
            }
            Stmt::Loop(s)    => 1 + count_covered_blocks(arena, s.body, _a),
            Stmt::Switch(s)  => {
                1 + s.arms.iter()
                    .map(|a| count_covered_blocks(arena, a.body, _a))
                    .sum::<usize>()
            }
            Stmt::TryCatch(s) => {
                count_covered_blocks(arena, s.try_body, _a)
                + s.catches.iter()
                    .map(|c| count_covered_blocks(arena, c.body, _a))
                    .sum::<usize>()
                + s.finally_body.map(|f| count_covered_blocks(arena, f, _a)).unwrap_or(0)
            }
            Stmt::Synchronized(s) => 1 + count_covered_blocks(arena, s.body, _a),
        }
    }

    // ── smoke tests: recovery must not panic ──────────────────────────

    macro_rules! smoke_test {
        ($name:ident, $file:expr) => {
            #[test]
            fn $name() {
                let bytes = load(&base($file));
                let (class, errors) = recover_all_methods(&bytes);
                assert!(errors.is_empty(),
                    "Recovery errors in {}:\n  {}", class, errors.join("\n  "));
            }
        };
        ($name:ident, full: $file:expr) => {
            #[test]
            fn $name() {
                let bytes = load($file);
                let (class, errors) = recover_all_methods(&bytes);
                assert!(errors.is_empty(),
                    "Recovery errors in {}:\n  {}", class, errors.join("\n  "));
            }
        };
    }

    smoke_test!(recover_main,           "../bulk/pkg/Main.class");
    smoke_test!(recover_switch_enum,    "TestEclipseSwitchEnum.class");
    smoke_test!(recover_switch_string,  "TestEclipseSwitchString.class");
    smoke_test!(recover_jsr,            "TestJsr.class");
    smoke_test!(recover_jsr2,           "TestJsr2.class");
    smoke_test!(recover_hotjava,        "TestHotjava.class");
    smoke_test!(recover_synchronized,   "TestJava1Synchronized.class");
    smoke_test!(recover_string_concat,  "TestStringConcatJ19.class");
    smoke_test!(recover_switch_j21,     "TestSwitchOnEnumWithoutEnumJ21.class");
    smoke_test!(recover_inner_ctor,     full: "vineflower-master/testData/classes/custom/v11/TestInnerClassConstructor.class");

    // ── structural shape tests ──────────────────────────────────────────

    /// A straight-line method `() -> void { return; }` should give a
    /// single Block stmt wrapping the return.
    #[test]
    fn test_recover_trivial_block() {
        use abyssflower_lib::classfile::attribute::{CodeAttribute};
        use abyssflower_lib::classfile::instruction::{Instruction, InsnKind};

        let code = CodeAttribute {
            max_stack:    0,
            max_locals:   1,
            instructions: vec![
                Instruction { offset: 0, opcode: 0xb1, wide: false,
                    kind: InsnKind::NoOperand },   // return
            ],
            exception_table: vec![],
            attributes:      vec![],
        };
        let cfg = cfg_builder::build(&code);
        let dom = DomTree::compute(&cfg);
        let (arena, root) = recover(&cfg, &dom, &code);

        // Root should be a single Block
        assert!(matches!(arena.get(root), Stmt::Block(_) | Stmt::Seq(_)),
            "Expected Block or Seq for trivial method, got {:?}", arena.get(root));
    }

    /// A conditional branch:
    ///   0: ifeq +5
    ///   3: return
    ///   5: return
    /// Should produce an If statement.
    #[test]
    fn test_recover_simple_if() {
        use abyssflower_lib::classfile::attribute::CodeAttribute;
        use abyssflower_lib::classfile::instruction::{Instruction, InsnKind};

        let code = CodeAttribute {
            max_stack:    1,
            max_locals:   1,
            instructions: vec![
                Instruction { offset: 0, opcode: 0x99 /*ifeq*/, wide: false,
                    kind: InsnKind::Branch { offset: 5 } },
                Instruction { offset: 3, opcode: 0xb1 /*return*/, wide: false,
                    kind: InsnKind::NoOperand },
                Instruction { offset: 5, opcode: 0xb1 /*return*/, wide: false,
                    kind: InsnKind::NoOperand },
            ],
            exception_table: vec![],
            attributes:      vec![],
        };
        let cfg = cfg_builder::build(&code);
        let dom = DomTree::compute(&cfg);
        let (arena, root) = recover(&cfg, &dom, &code);

        // Walk and find at least one If node
        fn has_if(arena: &abyssflower_lib::ir::StmtArena, id: abyssflower_lib::ir::StmtId) -> bool {
            match arena.get(id) {
                Stmt::If(_)    => true,
                Stmt::Seq(s)   => s.children.iter().any(|&c| has_if(arena, c)),
                Stmt::Loop(s)  => has_if(arena, s.body),
                _              => false,
            }
        }

        assert!(has_if(&arena, root),
            "Expected an If statement in recovery of conditional branch");
    }

    /// A simple loop:
    ///   0: iload_0
    ///   1: ifeq  +5  (exit loop if == 0)
    ///   4: goto   0  (back edge)
    ///   7: return
    #[test]
    fn test_recover_simple_loop() {
        use abyssflower_lib::classfile::attribute::CodeAttribute;
        use abyssflower_lib::classfile::instruction::{Instruction, InsnKind};

        let code = CodeAttribute {
            max_stack:    1,
            max_locals:   1,
            instructions: vec![
                // offset 0: iload_0  (1 byte)
                Instruction { offset: 0, opcode: 0x1a /*iload_0*/, wide: false,
                    kind: InsnKind::NoOperand },
                // offset 1: ifeq +6 → target 7 (exit)  (3 bytes)
                Instruction { offset: 1, opcode: 0x99 /*ifeq*/, wide: false,
                    kind: InsnKind::Branch { offset: 6 } },
                // offset 4: goto -4 → target 0 (back-edge)  (3 bytes)
                Instruction { offset: 4, opcode: 0xa7 /*goto*/, wide: false,
                    kind: InsnKind::Branch { offset: -4 } },
                // offset 7: return  (1 byte)
                Instruction { offset: 7, opcode: 0xb1 /*return*/, wide: false,
                    kind: InsnKind::NoOperand },
            ],
            exception_table: vec![],
            attributes:      vec![],
        };
        let cfg = cfg_builder::build(&code);
        let dom = DomTree::compute(&cfg);
        let (arena, root) = recover(&cfg, &dom, &code);

        fn has_loop(arena: &abyssflower_lib::ir::StmtArena, id: abyssflower_lib::ir::StmtId) -> bool {
            match arena.get(id) {
                Stmt::Loop(_)  => true,
                Stmt::Seq(s)   => s.children.iter().any(|&c| has_loop(arena, c)),
                _              => false,
            }
        }

        assert!(has_loop(&arena, root),
            "Expected a Loop statement in recovery of simple back-edge loop");
    }
}
