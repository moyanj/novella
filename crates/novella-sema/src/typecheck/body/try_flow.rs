//! try/catch/finally 的正常、返回、循环跳转与异常赋值状态。
use super::*;
use crate::typecheck::flow;

type States = Vec<HashSet<Span>>;

pub(super) fn check_try(sema: &mut Sema, ctx: &mut BodyCtx, node: &novella_ast::stmt::TryStmt) {
    let inferred_start = ctx
        .inferred_returns
        .as_ref()
        .map(|returns| returns.borrow().len());
    if node.catches.is_empty() && node.finally.is_none() {
        sema.type_error(ctx.module, node.span, "try 必须有 catch 或 finally");
    }
    let returns: std::rc::Rc<std::cell::RefCell<States>> = Default::default();
    let exceptions: std::rc::Rc<std::cell::RefCell<States>> = Default::default();
    let loop_start = ctx.loop_assignments.as_ref().map(|states| {
        let states = states.borrow();
        (states.breaks.len(), states.continues.len())
    });
    let mut body = fork_scope(ctx, ctx.scope);
    body.return_states = Some(returns.clone());
    body.exception_states = Some(exceptions.clone());
    check_block(sema, &mut body, &node.block);
    let mut normal = HashSet::new();
    if flow::block_falls_through(sema, ctx.module, &node.block) {
        normal.extend(body.uninitialized);
    }
    let thrown = exceptions.take();
    let catch_entry: HashSet<_> = thrown.iter().flatten().copied().collect();
    let mut previous = Vec::new();
    let mut catches_all = false;
    for catch in &node.catches {
        let scope = sema.scopes.new_scope(Some(ctx.scope), ScopeKind::Block);
        let mut branch = fork_scope(ctx, scope);
        branch.uninitialized = catch_entry.clone();
        branch.reachable &= !thrown.is_empty() && !catches_all;
        branch.return_states = Some(returns.clone());
        branch.exception_states = Some(exceptions.clone());
        branch.in_catch = true;
        let ty = match &catch.ty {
            Some(annotation) => {
                let ty = sema.resolve_type(ctx.module, ctx.scope, annotation, &ctx.type_env);
                if !sema.is_subclass(&crate::ty::exception_type(), &ty) && !ty.is_opaque() {
                    sema.type_error(
                        ctx.module,
                        annotation.span(),
                        format!("catch 类型必须是 Exception 子类，实为 `{ty}`"),
                    );
                }
                ty
            }
            None => crate::ty::exception_type(),
        };
        if previous.iter().any(|prior| sema.is_assignable(prior, &ty)) {
            sema.type_error(ctx.module, catch.span, "该 catch 已被先前的异常类型覆盖");
        }
        catches_all |= crate::ty::ty_eq(&ty, &crate::ty::exception_type());
        previous.push(ty.clone());
        let name_span = sema.modules[ctx.module]
            .tokens
            .iter()
            .find(|token| {
                token.span.start >= catch.span.start
                    && token.span.end <= catch.block.span.start
                    && token.slice == catch.name
            })
            .map(|token| token.span)
            .expect("catch 名字来自已解析 token");
        define_local(
            sema,
            &branch,
            &catch.name,
            SymbolKind::Variable {
                is_const: true,
                ty: Some(ty),
            },
            name_span,
        );
        let reachable = branch.reachable;
        check_block(sema, &mut branch, &catch.block);
        if reachable && flow::block_falls_through(sema, ctx.module, &catch.block) {
            normal.extend(branch.uninitialized);
        }
    }
    if !catches_all {
        exceptions.borrow_mut().extend(thrown);
    }
    // finally 的入口包含所有真实出口；中途调用可能抛出的位置单独保留。
    let loop_end = ctx.loop_assignments.as_ref().map(|states| {
        let states = states.borrow();
        (states.breaks.len(), states.continues.len())
    });
    let mut incoming = normal.clone();
    incoming.extend(returns.borrow().iter().flatten().copied());
    incoming.extend(exceptions.borrow().iter().flatten().copied());
    if let (Some(states), Some((breaks, continues))) = (&ctx.loop_assignments, loop_start) {
        let states = states.borrow();
        incoming.extend(states.breaks[breaks..].iter().flatten().copied());
        incoming.extend(states.continues[continues..].iter().flatten().copied());
    }
    let mut initialized = HashSet::new();
    let mut resumes = true;
    if let Some(finally) = &node.finally {
        let inferred_end = ctx
            .inferred_returns
            .as_ref()
            .map(|returns| returns.borrow().len());
        ctx.uninitialized = incoming.clone();
        check_block(sema, ctx, finally);
        initialized.extend(incoming.difference(&ctx.uninitialized).copied());
        resumes = flow::block_falls_through(sema, ctx.module, finally);
        if !resumes
            && let (Some(returns), Some(start), Some(end)) =
                (&ctx.inferred_returns, inferred_start, inferred_end)
        {
            // finally 覆盖原有 return，保留 finally 自身产生的返回类型。
            returns.borrow_mut().drain(start..end);
        }
    }
    if let (Some(states), Some((bs, cs)), Some((be, ce))) =
        (&ctx.loop_assignments, loop_start, loop_end)
    {
        let mut states = states.borrow_mut();
        if resumes {
            for state in &mut states.breaks[bs..be] {
                state.retain(|slot| !initialized.contains(slot));
            }
            for state in &mut states.continues[cs..ce] {
                state.retain(|slot| !initialized.contains(slot));
            }
        } else {
            // finally 自身新增的跳转保留，先前待执行的跳转被覆盖。
            states.breaks.drain(bs..be);
            states.continues.drain(cs..ce);
        }
    }
    ctx.uninitialized = normal.difference(&initialized).copied().collect();
    if resumes {
        if let Some(parent) = &ctx.return_states {
            parent
                .borrow_mut()
                .extend(returns.take().into_iter().map(|mut state| {
                    state.retain(|slot| !initialized.contains(slot));
                    state
                }));
        }
        if let Some(parent) = &ctx.exception_states {
            parent
                .borrow_mut()
                .extend(exceptions.take().into_iter().map(|mut state| {
                    state.retain(|slot| !initialized.contains(slot));
                    state
                }));
        }
    }
}
