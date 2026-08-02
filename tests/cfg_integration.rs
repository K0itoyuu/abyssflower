//! Integration tests for the CFG module.
//!
//! We parse real class files and verify predecessor/successor invariants.

#[cfg(test)]
mod cfg_tests {
    use abyssflower_lib::cfg::{
        builder, find_natural_loops, BasicBlock, Cfg, DomTree, PostDomTree, ENTRY_BLOCK, EXIT_BLOCK,
    };
    use abyssflower_lib::ClassFile;

    /// Load a test .class file, returning its bytes.
    fn load_class(name: &str) -> Vec<u8> {
        let path = format!("vineflower-master/testData/classes/custom/{}", name);
        std::fs::read(&path).unwrap_or_else(|_| panic!("Cannot read test class: {}", path))
    }

    /// Build CFGs for every method in a class file and run sanity checks.
    fn check_class(bytes: &[u8]) {
        let cf = ClassFile::parse(bytes).expect("parse failed");

        for method in &cf.methods {
            let code = match method.code() {
                Some(c) => c,
                None => continue,
            };

            let cfg = builder::build(code);

            // 1. ENTRY and EXIT always exist
            let _ = cfg.entry();
            let _ = cfg.exit();

            // 2. Every block referenced in a succ/pred list actually exists
            for block in cfg.blocks.iter() {
                for &succ in &block.succs {
                    assert!(
                        cfg.blocks.iter().any(|b| b.id == succ),
                        "succ {} of block {} not found in CFG for {}.{}",
                        succ,
                        block.id,
                        cf.this_class,
                        method.name
                    );
                }
                for &pred in &block.preds {
                    assert!(
                        cfg.blocks.iter().any(|b| b.id == pred),
                        "pred {} of block {} not found in CFG for {}.{}",
                        pred,
                        block.id,
                        cf.this_class,
                        method.name
                    );
                }
            }

            // 3. Pred/succ symmetry: if A → B then B has A in preds
            for block in cfg.blocks.iter() {
                for &succ in &block.succs {
                    let succ_block = cfg.block(succ);
                    assert!(
                        succ_block.preds.contains(&block.id),
                        "symmetry broken: {}->{} but {} missing from preds of {}",
                        block.id,
                        succ,
                        block.id,
                        succ
                    );
                }
            }

            // 4. Instruction count matches what was parsed
            let total_insns: usize = cfg.blocks.iter().map(|b| b.instructions.len()).sum();
            assert_eq!(
                total_insns,
                code.instructions.len(),
                "instruction count mismatch for {}.{}",
                cf.this_class,
                method.name
            );

            // 5. Dominator tree is computable without panic
            if cfg.blocks.len() > 2 {
                let dom = DomTree::compute(&cfg);
                // ENTRY dominates itself
                assert!(dom.dominates(ENTRY_BLOCK, ENTRY_BLOCK));
                // ENTRY dominates every reachable block
                for id in cfg.rpo() {
                    assert!(
                        dom.dominates(ENTRY_BLOCK, id),
                        "ENTRY does not dominate block {} in {}.{}",
                        id,
                        cf.this_class,
                        method.name
                    );
                }
            }
        }
    }

    fn diamond_cfg() -> Cfg {
        let mut entry = BasicBlock::synthetic(ENTRY_BLOCK);
        entry.succs = vec![1];
        let mut head = BasicBlock::new(1, vec![]);
        head.preds = vec![ENTRY_BLOCK];
        head.succs = vec![2, 3];
        let mut left = BasicBlock::new(2, vec![]);
        left.preds = vec![1];
        left.succs = vec![4];
        let mut right = BasicBlock::new(3, vec![]);
        right.preds = vec![1];
        right.succs = vec![4];
        let mut join = BasicBlock::new(4, vec![]);
        join.preds = vec![2, 3];
        join.succs = vec![EXIT_BLOCK];
        let mut exit = BasicBlock::synthetic(EXIT_BLOCK);
        exit.preds = vec![4];
        Cfg {
            blocks: vec![entry, head, left, right, join, exit],
            exception_ranges: vec![],
        }
    }

    #[test]
    fn test_post_dominator_diamond() {
        let post_dom = PostDomTree::compute(&diamond_cfg());
        assert_eq!(post_dom.immediately_post_dominates(1), Some(4));
        assert_eq!(post_dom.immediately_post_dominates(2), Some(4));
        assert_eq!(post_dom.immediately_post_dominates(3), Some(4));
        assert_eq!(post_dom.immediately_post_dominates(4), Some(EXIT_BLOCK));
        assert!(post_dom.post_dominates(4, 1));
        assert!(!post_dom.post_dominates(2, 1));
    }

    #[test]
    fn test_post_dominator_excludes_region_without_exit_path() {
        let mut cfg = diamond_cfg();
        let mut first = BasicBlock::new(5, vec![]);
        first.preds = vec![6];
        first.succs = vec![6];
        let mut second = BasicBlock::new(6, vec![]);
        second.preds = vec![5];
        second.succs = vec![5];
        cfg.blocks.insert(cfg.blocks.len() - 1, first);
        cfg.blocks.insert(cfg.blocks.len() - 1, second);

        let post_dom = PostDomTree::compute(&cfg);
        assert_eq!(post_dom.immediately_post_dominates(5), None);
        assert!(!post_dom.post_dominates(EXIT_BLOCK, 5));
        assert!(!post_dom.blocks().contains(&5));
    }

    #[test]
    fn test_post_dominator_nested_branches_choose_nearest_join() {
        let mut entry = BasicBlock::synthetic(ENTRY_BLOCK);
        entry.succs = vec![1];
        let mut outer = BasicBlock::new(1, vec![]);
        outer.preds = vec![ENTRY_BLOCK];
        outer.succs = vec![2, 5];
        let mut inner = BasicBlock::new(2, vec![]);
        inner.preds = vec![1];
        inner.succs = vec![3, 4];
        let mut inner_left = BasicBlock::new(3, vec![]);
        inner_left.preds = vec![2];
        inner_left.succs = vec![6];
        let mut inner_right = BasicBlock::new(4, vec![]);
        inner_right.preds = vec![2];
        inner_right.succs = vec![6];
        let mut outer_right = BasicBlock::new(5, vec![]);
        outer_right.preds = vec![1];
        outer_right.succs = vec![7];
        let mut inner_join = BasicBlock::new(6, vec![]);
        inner_join.preds = vec![3, 4];
        inner_join.succs = vec![7];
        let mut outer_join = BasicBlock::new(7, vec![]);
        outer_join.preds = vec![5, 6];
        outer_join.succs = vec![EXIT_BLOCK];
        let mut exit = BasicBlock::synthetic(EXIT_BLOCK);
        exit.preds = vec![7];
        let cfg = Cfg {
            blocks: vec![
                entry,
                outer,
                inner,
                inner_left,
                inner_right,
                outer_right,
                inner_join,
                outer_join,
                exit,
            ],
            exception_ranges: vec![],
        };

        let post_dom = PostDomTree::compute(&cfg);
        assert_eq!(post_dom.immediately_post_dominates(2), Some(6));
        assert_eq!(post_dom.immediately_post_dominates(1), Some(7));
        assert_eq!(post_dom.immediately_post_dominates(6), Some(7));
        assert!(post_dom.post_dominates(7, 3));
    }

    #[test]
    fn test_post_dominator_loop_uses_loop_exit() {
        let mut entry = BasicBlock::synthetic(ENTRY_BLOCK);
        entry.succs = vec![1];
        let mut header = BasicBlock::new(1, vec![]);
        header.preds = vec![ENTRY_BLOCK, 2];
        header.succs = vec![2, 3];
        let mut body = BasicBlock::new(2, vec![]);
        body.preds = vec![1];
        body.succs = vec![1];
        let mut after = BasicBlock::new(3, vec![]);
        after.preds = vec![1];
        after.succs = vec![EXIT_BLOCK];
        let mut exit = BasicBlock::synthetic(EXIT_BLOCK);
        exit.preds = vec![3];
        let cfg = Cfg {
            blocks: vec![entry, header, body, after, exit],
            exception_ranges: vec![],
        };

        let post_dom = PostDomTree::compute(&cfg);
        assert_eq!(post_dom.immediately_post_dominates(1), Some(3));
        assert_eq!(post_dom.immediately_post_dominates(2), Some(1));
        assert!(post_dom.post_dominates(3, 2));
    }

    #[test]
    fn test_post_dominator_includes_exception_edges() {
        let mut entry = BasicBlock::synthetic(ENTRY_BLOCK);
        entry.succs = vec![1];
        let mut protected = BasicBlock::new(1, vec![]);
        protected.preds = vec![ENTRY_BLOCK];
        protected.succs = vec![2];
        protected.succ_exceptions = vec![4];
        let mut normal = BasicBlock::new(2, vec![]);
        normal.preds = vec![1];
        normal.succs = vec![3];
        let mut join = BasicBlock::new(3, vec![]);
        join.preds = vec![2, 4];
        join.succs = vec![EXIT_BLOCK];
        let mut handler = BasicBlock::new(4, vec![]);
        handler.pred_exceptions = vec![1];
        handler.succs = vec![3];
        let mut exit = BasicBlock::synthetic(EXIT_BLOCK);
        exit.preds = vec![3];
        let cfg = Cfg {
            blocks: vec![entry, protected, normal, join, handler, exit],
            exception_ranges: vec![],
        };

        let post_dom = PostDomTree::compute(&cfg);
        assert_eq!(post_dom.immediately_post_dominates(1), Some(3));
        assert!(post_dom.post_dominates(3, 1));
        assert!(!post_dom.post_dominates(2, 1));
    }

    #[test]
    fn test_natural_loop_includes_try_block_reaching_back_edge_via_catch() {
        let mut entry = BasicBlock::synthetic(ENTRY_BLOCK);
        entry.succs = vec![1];

        let mut header = BasicBlock::new(1, vec![]);
        header.preds = vec![ENTRY_BLOCK, 4];
        header.succs = vec![2, 5];

        let mut protected = BasicBlock::new(2, vec![]);
        protected.preds = vec![1];
        protected.succs = vec![EXIT_BLOCK];
        protected.succ_exceptions = vec![3];

        let mut handler = BasicBlock::new(3, vec![]);
        handler.pred_exceptions = vec![2];
        handler.succs = vec![4];

        let mut tail = BasicBlock::new(4, vec![]);
        tail.preds = vec![3];
        tail.succs = vec![1];

        let mut after = BasicBlock::new(5, vec![]);
        after.preds = vec![1];
        after.succs = vec![EXIT_BLOCK];

        let mut exit = BasicBlock::synthetic(EXIT_BLOCK);
        exit.preds = vec![2, 5];

        let cfg = Cfg {
            blocks: vec![entry, header, protected, handler, tail, after, exit],
            exception_ranges: vec![],
        };
        let dom = DomTree::compute(&cfg);
        let loops = find_natural_loops(&cfg, &dom);

        assert_eq!(loops.len(), 1);
        assert!(loops[0].body.contains(&2), "protected try block missing");
        assert!(loops[0].body.contains(&3), "catch handler missing");
    }

    #[test]
    fn test_cfg_repository_fixture() {
        check_class(include_bytes!(
            "java_classes/fixture/ControlFlowFixture.class"
        ));
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_cfg_main_class() {
        check_class(&load_class("../bulk/pkg/Main.class"));
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_cfg_switch_enum() {
        check_class(&load_class("TestEclipseSwitchEnum.class"));
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_cfg_switch_string() {
        check_class(&load_class("TestEclipseSwitchString.class"));
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_cfg_jsr() {
        check_class(&load_class("TestJsr.class"));
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_cfg_jsr2() {
        check_class(&load_class("TestJsr2.class"));
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_cfg_hotjava() {
        check_class(&load_class("TestHotjava.class"));
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_cfg_java1_synchronized() {
        check_class(&load_class("TestJava1Synchronized.class"));
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_cfg_inner_constructor() {
        let bytes = std::fs::read(
            "vineflower-master/testData/classes/custom/v11/TestInnerClassConstructor.class",
        )
        .expect("cannot read");
        check_class(&bytes);
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_cfg_string_concat_j19() {
        check_class(&load_class("TestStringConcatJ19.class"));
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_cfg_switch_enum_j21() {
        check_class(&load_class("TestSwitchOnEnumWithoutEnumJ21.class"));
    }

    // ── manual CFG shape tests ─────────────────────────────────────────

    /// Simple straight-line method: `() -> void { return; }`
    /// Expected CFG: ENTRY → B1(return) → EXIT
    #[test]
    fn test_cfg_straight_line() {
        use abyssflower_lib::classfile::attribute::CodeAttribute;
        use abyssflower_lib::classfile::instruction::{InsnKind, Instruction};

        let code = CodeAttribute {
            max_stack: 1,
            max_locals: 1,
            instructions: vec![Instruction {
                offset: 0,
                opcode: 0xb1, /*return*/
                wide: false,
                kind: InsnKind::NoOperand,
            }],
            exception_table: vec![],
            attributes: vec![],
        };

        let cfg = builder::build(&code);
        // ENTRY + 1 real block + EXIT = 3
        assert_eq!(cfg.blocks.len(), 3);
        // ENTRY → real block → EXIT
        assert_eq!(cfg.entry().succs.len(), 1);
        let real_id = cfg.entry().succs[0];
        let real = cfg.block(real_id);
        assert_eq!(real.succs.len(), 1);
        assert_eq!(real.succs[0], EXIT_BLOCK);
    }

    /// Two-branch if: ifeq → taken / fall-through
    #[test]
    fn test_cfg_conditional_branch() {
        use abyssflower_lib::classfile::attribute::CodeAttribute;
        use abyssflower_lib::classfile::instruction::{InsnKind, Instruction};

        // Bytecode:
        //  0: ifeq  +5  (jump to offset 5)  3 bytes
        //  3: return               (fall-through)    1 byte
        //  5: return               (branch target)   1 byte
        let code = CodeAttribute {
            max_stack: 1,
            max_locals: 1,
            instructions: vec![
                Instruction {
                    offset: 0,
                    opcode: 0x99, /*ifeq*/
                    wide: false,
                    kind: InsnKind::Branch { offset: 5 },
                }, // target = 0 + 5 = 5
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

        let cfg = builder::build(&code);
        // ENTRY + 3 real blocks + EXIT = 5
        assert_eq!(
            cfg.blocks.len(),
            5,
            "expected 5 blocks, got {}\n{}",
            cfg.blocks.len(),
            cfg.dump()
        );

        // The ifeq block must have 2 successors
        let ifeq_block = cfg
            .real_blocks()
            .find(|b| b.start_offset == 0)
            .expect("ifeq block not found");
        assert_eq!(
            ifeq_block.succs.len(),
            2,
            "ifeq block should have 2 successors"
        );
    }
}
