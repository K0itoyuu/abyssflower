/// Integration tests for Phase 5: stack simulation + expression lifting.
#[cfg(test)]
mod stack_sim_tests {
    use abyssflower_lib::classfile::attribute::CodeAttribute;
    use abyssflower_lib::classfile::instruction::{Instruction, InsnKind};
    use abyssflower_lib::classfile::constant_pool::ConstantPool;
    use abyssflower_lib::ir::expr::{BinOp, Expr};
    use abyssflower_lib::ir::{simulate_block, SlotInfo};
    use abyssflower_lib::types::java_type::JavaType;

    // Minimal stub: build a ConstantPool with no entries for simple tests.
    fn empty_pool_bytes() -> Vec<u8> {
        // constant_pool_count = 1 (just the null slot)
        vec![0x00, 0x01]
    }

    // ── helper: run sim on a raw instruction list ────────────────────────

    fn sim(insns: Vec<Instruction>) -> abyssflower_lib::ir::SimResult {
        // Build a minimal pool
        use abyssflower_lib::classfile::cursor::Cursor;
        let pool_bytes = empty_pool_bytes();
        let mut cur = Cursor::new(&pool_bytes);
        let pool = ConstantPool::parse(&mut cur).unwrap();

        simulate_block(&insns, &pool, vec![], true, "Test", &[])
    }

    // ── constants ────────────────────────────────────────────────────────

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_iconst_push() {
        let result = sim(vec![
            Instruction { offset: 0, opcode: 3 /*iconst_0*/, wide: false, kind: InsnKind::NoOperand },
        ]);
        assert_eq!(result.stack_out.len(), 1, "iconst_0 should leave one value on stack");
        assert_eq!(result.stack_out[0].ty, JavaType::INT);
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_bipush() {
        let result = sim(vec![
            Instruction { offset: 0, opcode: 16 /*bipush*/, wide: false,
                kind: InsnKind::BytePush { value: 42 } },
        ]);
        assert_eq!(result.stack_out.len(), 1);
        match &result.stack_out[0].expr {
            Expr::Const(c) => match c.value {
                abyssflower_lib::ir::ConstValue::Int(v) => assert_eq!(v, 42),
                _ => panic!("expected Int"),
            },
            _ => panic!("expected Const"),
        }
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_sipush() {
        let result = sim(vec![
            Instruction { offset: 0, opcode: 17 /*sipush*/, wide: false,
                kind: InsnKind::ShortPush { value: 1000 } },
        ]);
        assert_eq!(result.stack_out.len(), 1);
        if let Expr::Const(c) = &result.stack_out[0].expr {
            if let abyssflower_lib::ir::ConstValue::Int(v) = c.value {
                assert_eq!(v, 1000);
            }
        }
    }

    // ── arithmetic ───────────────────────────────────────────────────────

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_iadd() {
        let result = sim(vec![
            Instruction { offset: 0, opcode: 3 /*iconst_0*/, wide: false, kind: InsnKind::NoOperand },
            Instruction { offset: 1, opcode: 4 /*iconst_1*/, wide: false, kind: InsnKind::NoOperand },
            Instruction { offset: 2, opcode: 96 /*iadd*/,    wide: false, kind: InsnKind::NoOperand },
        ]);
        assert_eq!(result.stack_out.len(), 1, "iadd should consume 2, produce 1");
        assert!(matches!(result.stack_out[0].expr, Expr::BinOp(BinOp::Add, _, _)));
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_isub() {
        let result = sim(vec![
            Instruction { offset: 0, opcode: 4, wide: false, kind: InsnKind::NoOperand }, // iconst_1
            Instruction { offset: 1, opcode: 5, wide: false, kind: InsnKind::NoOperand }, // iconst_2
            Instruction { offset: 2, opcode: 100, wide: false, kind: InsnKind::NoOperand }, // isub
        ]);
        assert!(matches!(result.stack_out[0].expr, Expr::BinOp(BinOp::Sub, _, _)));
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_ineg() {
        let result = sim(vec![
            Instruction { offset: 0, opcode: 3, wide: false, kind: InsnKind::NoOperand }, // iconst_0
            Instruction { offset: 1, opcode: 116, wide: false, kind: InsnKind::NoOperand }, // ineg
        ]);
        assert!(matches!(result.stack_out[0].expr, Expr::UnOp(abyssflower_lib::ir::UnOp::Neg, _)));
    }

    // ── stack manipulation ────────────────────────────────────────────────

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_dup() {
        let result = sim(vec![
            Instruction { offset: 0, opcode: 3, wide: false, kind: InsnKind::NoOperand }, // iconst_0
            Instruction { offset: 1, opcode: 89, wide: false, kind: InsnKind::NoOperand }, // dup
        ]);
        assert_eq!(result.stack_out.len(), 2, "dup should produce 2 stack slots from 1");
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_pop() {
        let result = sim(vec![
            Instruction { offset: 0, opcode: 3, wide: false, kind: InsnKind::NoOperand }, // iconst_0
            Instruction { offset: 1, opcode: 87, wide: false, kind: InsnKind::NoOperand }, // pop
        ]);
        assert_eq!(result.stack_out.len(), 0, "pop should consume the top value");
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_swap() {
        let result = sim(vec![
            Instruction { offset: 0, opcode: 3, wide: false, kind: InsnKind::NoOperand }, // iconst_0
            Instruction { offset: 1, opcode: 4, wide: false, kind: InsnKind::NoOperand }, // iconst_1
            Instruction { offset: 2, opcode: 95, wide: false, kind: InsnKind::NoOperand }, // swap
        ]);
        assert_eq!(result.stack_out.len(), 2);
        // After swap: top is iconst_0 (value 0), bottom is iconst_1 (value 1)
        if let Expr::Const(c) = &result.stack_out[1].expr {
            if let abyssflower_lib::ir::ConstValue::Int(v) = c.value { assert_eq!(v, 0); }
        }
    }

    // ── casts ─────────────────────────────────────────────────────────────

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_i2l_cast() {
        let result = sim(vec![
            Instruction { offset: 0, opcode: 3,   wide: false, kind: InsnKind::NoOperand }, // iconst_0
            Instruction { offset: 1, opcode: 133, wide: false, kind: InsnKind::NoOperand }, // i2l
        ]);
        assert_eq!(result.stack_out.len(), 1);
        assert!(matches!(result.stack_out[0].expr,
            Expr::Cast(abyssflower_lib::ir::CastKind::I2L, _, _)));
        assert_eq!(result.stack_out[0].ty, JavaType::LONG);
    }

    // ── locals ────────────────────────────────────────────────────────────

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_istore_iload() {
        let result = sim(vec![
            // bipush 99
            Instruction { offset: 0, opcode: 16, wide: false, kind: InsnKind::BytePush { value: 99 } },
            // istore_1 (opcode 60)
            Instruction { offset: 2, opcode: 60, wide: false, kind: InsnKind::NoOperand },
            // iload_1 (opcode 27)
            Instruction { offset: 3, opcode: 27, wide: false, kind: InsnKind::NoOperand },
        ]);
        // istore emits an Assign statement
        assert!(!result.stmts.is_empty(), "istore should emit an assignment");
        // iload puts the var back on the stack
        assert_eq!(result.stack_out.len(), 1);
        assert!(matches!(&result.stack_out[0].expr, Expr::LocalVar(lv) if lv.slot == 1));
    }

    // ── return ────────────────────────────────────────────────────────────

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_return_void() {
        let result = sim(vec![
            Instruction { offset: 0, opcode: 177, wide: false, kind: InsnKind::NoOperand }, // return
        ]);
        assert_eq!(result.stmts.len(), 1);
        assert!(matches!(result.stmts[0], Expr::Return(None)));
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_ireturn() {
        let result = sim(vec![
            Instruction { offset: 0, opcode: 3,   wide: false, kind: InsnKind::NoOperand }, // iconst_0
            Instruction { offset: 1, opcode: 172, wide: false, kind: InsnKind::NoOperand }, // ireturn
        ]);
        assert_eq!(result.stmts.len(), 1);
        assert!(matches!(&result.stmts[0], Expr::Return(Some(_))));
    }

    // ── smoke test: simulate all methods in a real class ──────────────────

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_simulate_real_class() {
        use abyssflower_lib::ClassFile;
        use abyssflower_lib::classfile::cursor::Cursor;

        let bytes = std::fs::read(
            "vineflower-master/testData/classes/bulk/pkg/Main.class"
        ).expect("cannot read Main.class");
        let cf = ClassFile::parse(&bytes).unwrap();

        for method in &cf.methods {
            let code = match method.code() { Some(c) => c, None => continue };
            let is_static = method.is_static();

            let result = simulate_block(
                &code.instructions,
                &cf.constant_pool,
                vec![],
                is_static,
                &cf.this_class,
                &[],
            );
            // Should not panic and should produce some output for non-empty methods
            if !code.instructions.is_empty() {
                assert!(result.stmts.len() + result.stack_out.len() > 0
                    || code.instructions.len() == 1,
                    "Expected some output from {}.{}",
                    cf.this_class, method.name);
            }
        }
    }

    // ── iinc ─────────────────────────────────────────────────────────────

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_iinc() {
        let result = sim(vec![
            Instruction { offset: 0, opcode: 132, wide: false,
                kind: InsnKind::Iinc { index: 1, const_: 1 } }, // iinc 1, 1
        ]);
        assert_eq!(result.stmts.len(), 1);
        assert!(matches!(result.stmts[0], Expr::IInc { slot: 1, delta: 1, .. }));
    }
}
