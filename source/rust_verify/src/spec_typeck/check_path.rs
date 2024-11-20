use rustc_hir::def_id::DefId;
use rustc_hir::hir_id::HirId;
use rustc_hir::{PrimTy, QPath, GenericArg, PathSegment};
use rustc_hir::def::{Res, DefKind, CtorOf, CtorKind};
use rustc_hir::def_id::LocalDefId;
use vir::ast::{Typ, Typs, VirErr, TypX};
use crate::spec_typeck::State;
use std::sync::Arc;
use rustc_span::Span;
use rustc_middle::ty::{Ty, TyCtxt, GenericParamDef, Region, Const, GenericPredicates, AdtDef, Generics, PolyTraitRef, GenericParamDefKind, TermKind, TyKind};
use rustc_infer::infer::InferCtxt;
use rustc_errors::ErrorGuaranteed;
use rustc_hir_analysis::hir_ty_lowering::{GenericPathSegment, IsMethodCall, GenericArgCountMismatch, HirTyLowerer};
use rustc_hir_analysis::hir_ty_lowering::generics::check_generic_arg_count_for_call;
use crate::util::err_span;
use rustc_hir::GenericArgsParentheses;
use std::collections::HashSet;
use super::method_probe::MethodResult;

/// This file is for translating QPaths. This is complicated because there's lot of different
/// types of paths which may have type arguments along various segments. We need to translate
/// those type arguments and map them to the correct generic params of whatever the item is.
///
/// We also handle TypeRelative paths here, and method calls.

#[derive(Debug)]
pub enum PathResolution {
    Local(HirId),
    Fn(DefId, Typs),
    Const(DefId),
    Datatype(DefId, Typs),
    DatatypeVariant(DefId, Typs),
    PrimTy(PrimTy),
    TyParam(vir::ast::Ident),
    AssocTy(DefId, Typs, Typs),
}

impl<'a, 'tcx> State<'a, 'tcx> {
    pub fn lookup_method_call(
        &mut self,
        path_segment: &'tcx PathSegment,
        self_typ: &Typ,
        span: Span,
        expr: &'tcx rustc_hir::Expr<'tcx>,
    ) -> Result<(MethodResult, Typs), VirErr> {
        let self_typ = self.get_typ_as_concrete_as_possible(self_typ)?;
        let (term, infcx) = self.vir_ty_to_middle(span, &self_typ);
        let self_ty = match term.unpack() {
            TermKind::Ty(ty) => ty,
            TermKind::Const(_) => {
                return err_span(span, "unexpected Const here");
            }
        };
        if matches!(self_ty.kind(), TyKind::Infer(_)) {
            todo!();
        }
        let mres = crate::spec_typeck::method_probe::lookup_method(
            self.tcx, self_ty, path_segment, span,
            expr, 
            self.param_env,
            self.bctx.fun_id.expect_local(),
            infcx)?;

        let typ_args = self.check_method_call_generics(mres.def_id, path_segment)?;
        Ok((mres, Arc::new(typ_args)))
    }

    pub fn check_qpath_for_expr(
        &mut self,
        qpath: &QPath<'tcx>,
        expr_hir_id: HirId,
    ) -> Result<PathResolution, VirErr> {
        match qpath {
            QPath::Resolved(qualified_self, path) => {
                self.check_res(path.span, qualified_self, &path.res, path.segments)
            }
            QPath::TypeRelative(ty, path_segment) => {
                let t = self.check_ty(ty)?;
                let (term, infcx) = self.vir_ty_to_middle(ty.span, &t);
                let mty = match term.unpack() {
                    TermKind::Ty(ty) => ty,
                    TermKind::Const(_) => {
                        return err_span(ty.span, "unexpected Const here");
                    }
                };
                let (def_kind, def_id) = crate::spec_typeck::method_probe::resolve_fully_qualified_call(
                    self.tcx,
                    qpath.span(),
                    path_segment.ident,
                    mty,
                    ty.span,
                    expr_hir_id,
                    self.param_env,
                    self.bctx.fun_id.expect_local(),
                    infcx,
                )?;

                match def_kind {
                    DefKind::AssocFn => {
                        let typ_args = self.check_method_call_generics_with_self_type(def_id, path_segment, &t, ty.span)?;
                        Ok(PathResolution::Fn(def_id, Arc::new(typ_args)))
                    }
                    _ => {
                        dbg!(def_kind);
                        todo!()
                    }
                }
            }
            QPath::LangItem(..) => {
                todo!()
            }
        }
    }

    pub fn check_qpath_for_type(
        &mut self,
        qpath: &QPath<'tcx>,
    ) -> Result<PathResolution, VirErr> {
        match qpath {
            QPath::Resolved(qualified_self, path) => {
                self.check_res(path.span, qualified_self, &path.res, path.segments)
            }
            QPath::TypeRelative(_ty, _path_segment) => {
                todo!()
            }
            QPath::LangItem(..) => {
                todo!()
            }
        }
    }

    fn check_res(
        &mut self,
        span: Span,
        qualified_self: &Option<&'tcx rustc_hir::Ty<'tcx>>,
        res: &Res,
        segments: &'tcx [PathSegment],
    ) -> Result<PathResolution, VirErr> {
        match res {
            Res::Def(def_kind, def_id) => {
                match def_kind {
                    DefKind::Fn => {
                        let generic_params = self.check_path_generics(span, qualified_self, *def_kind, *def_id, segments)?;
                        Ok(PathResolution::Fn(*def_id, Arc::new(generic_params)))
                    }
                    DefKind::Struct => {
                        assert!(qualified_self.is_none());
                        let generic_params = self.check_path_generics_last_only(*def_id, segments)?;
                        Ok(PathResolution::Datatype(*def_id, Arc::new(generic_params)))
                    }
                    DefKind::Variant => {
                        assert!(qualified_self.is_none());
                        let generic_params = self.check_path_generics_penultimate_only(*def_id, segments)?;
                        Ok(PathResolution::DatatypeVariant(*def_id, Arc::new(generic_params)))
                    }
                    DefKind::Ctor(CtorOf::Struct, CtorKind::Fn | CtorKind::Const) => {
                        assert!(qualified_self.is_none());
                        let generic_params = self.check_path_generics(span, qualified_self, *def_kind, *def_id, segments)?;
                        let def_id = self.tcx.parent(*def_id);
                        Ok(PathResolution::Datatype(def_id, Arc::new(generic_params)))
                    }
                    DefKind::Ctor(CtorOf::Variant, CtorKind::Fn | CtorKind::Const) => {
                        assert!(qualified_self.is_none());
                        let generic_params = self.check_path_generics(span, qualified_self, *def_kind, *def_id, segments)?;
                        let def_id = self.tcx.parent(*def_id);
                        Ok(PathResolution::DatatypeVariant(def_id, Arc::new(generic_params)))
                    }
                    DefKind::TyParam | DefKind::ConstParam => {
                        assert!(qualified_self.is_none());
                        assert!(segments.len() == 1);
                        Ok(PathResolution::TyParam(Arc::new(segments[0].ident.to_string())))
                    }
                    DefKind::AssocTy => {
                        let (trait_typ_args, extra_typ_args) =
                            self.check_assoc_ty_generics(qualified_self.unwrap(), *def_id, &segments)?;
                        Ok(PathResolution::AssocTy(*def_id, Arc::new(trait_typ_args), Arc::new(extra_typ_args)))
                    }
                    _ => {
                        dbg!(def_kind);
                        todo!()
                    }
                }
            }
            Res::PrimTy(prim_ty) => Ok(PathResolution::PrimTy(*prim_ty)),
            Res::Local(id) => Ok(PathResolution::Local(*id)),
            _ => todo!(),
        }
    }

    pub fn check_method_call_generics(
        &mut self,
        def_id: DefId,
        path_segment: &'tcx PathSegment,
    ) -> Result<Vec<Typ>, VirErr> {
        let generics = self.tcx.generics_of(def_id);
        let mut v = self.check_segment_generics(None, path_segment, generics)?;

        let mut w = vec![];
        for _i in 0 .. generics.parent_count {
            w.push(self.new_unknown_typ());
        }
        w.append(&mut v);
        Ok(w)
    }

    pub fn check_method_call_generics_with_self_type(
        &mut self,
        def_id: DefId,
        path_segment: &'tcx PathSegment,
        self_typ: &Typ,
        span: Span,
    ) -> Result<Vec<Typ>, VirErr> {
        let generics = self.tcx.generics_of(def_id);
        let mut v = self.check_segment_generics(None, path_segment, generics)?;

        let mut w = vec![];
        for _i in 0 .. generics.parent_count {
            w.push(self.new_unknown_typ());
        }

        let self_typ2 = self.item_type_substitution(
            span,
            self.tcx.impl_of_method(def_id).unwrap(),
            &Arc::new(w.clone()))?;
        self.expect_exact(self_typ, &self_typ2)?;

        w.append(&mut v);
        Ok(w)
    }

    pub fn check_assoc_ty_generics(
      &mut self,
      qualified_self: &'tcx rustc_hir::Ty<'tcx>,
      def_id: DefId,
      segments: &'tcx [PathSegment]
    ) -> Result<(Vec<Typ>, Vec<Typ>), VirErr> {
        let trait_id = self.tcx.trait_of_item(def_id).unwrap();
        let generics = self.tcx.generics_of(def_id);
        let generics_parent = self.tcx.generics_of(trait_id);
        assert!(segments.len() == 2);
        let typs1 = self.check_segment_generics(
            Some(qualified_self),
            &segments[0],
            &generics_parent)?;
        let typs2 = self.check_segment_generics(
            None,
            &segments[1],
            &generics)?;
        Ok((typs1, typs2))
    }

    pub fn check_path_generics_last_only(
        &mut self,
        def_id: DefId,
        segments: &'tcx [PathSegment],
    ) -> Result<Vec<Typ>, VirErr> {
        for seg in segments.split_last().unwrap().1.iter() {
            if seg.args.is_some() {
                return err_span(seg.args.unwrap().span_ext, "unexpected generic arguments here");
            }
        }
        let generics = self.tcx.generics_of(def_id);
        self.check_segment_generics(None, &segments[segments.len() - 1], generics)
    }

    pub fn check_path_generics_penultimate_only(
        &mut self,
        def_id: DefId,
        segments: &'tcx [PathSegment],
    ) -> Result<Vec<Typ>, VirErr> {
        assert!(segments.len() >= 2);
        for (i, seg) in segments.iter().enumerate() {
            if i != segments.len() - 2 && seg.args.is_some() {
                return err_span(seg.args.unwrap().span_ext, "unexpected generic arguments here");
            }
        }
        let generics = self.tcx.generics_of(def_id);
        self.check_segment_generics(None, &segments[segments.len() - 2], generics)
    }

    pub fn check_path_generics(
        &mut self,
        span: Span,
        qualified_self: &Option<&'tcx rustc_hir::Ty<'tcx>>,
        def_kind: DefKind,
        def_id: DefId,
        segments: &'tcx [PathSegment],
    ) -> Result<Vec<Typ>, VirErr> {
        assert!(qualified_self.is_none());
        let generic_segments = self.lowerer().probe_generic_path_segments(
            segments, None, def_kind, def_id, span);
        let mut idx_set = HashSet::new();
        for GenericPathSegment(def_id, index) in &generic_segments {
            let seg = &segments[*index];
            let generics = self.tcx.generics_of(def_id);
            let arg_count = check_generic_arg_count_for_call(self.tcx, *def_id, generics, seg, IsMethodCall::No);
            if let Err(GenericArgCountMismatch { .. }) = arg_count.correct {
                return err_span(seg.args.unwrap().span_ext, "too many generic arguments here");
            }
            idx_set.insert(*index);
        }

        for i in 0 .. segments.len() {
            if !idx_set.contains(&i) {
                if segments[i].args.is_some() {
                    return err_span(segments[i].args.unwrap().span_ext, "unexpected generic arguments here");
                }
            }
        }

        let mut generic_params = vec![];
        for GenericPathSegment(def_id, index) in &generic_segments {
            let seg = &segments[*index];
            let generics = self.tcx.generics_of(def_id);
            generic_params.append(&mut self.check_segment_generics(None, seg, generics)?);
        }
        Ok(generic_params)
    }

    pub fn check_segment_generics(&mut self, qualified_self: Option<&'tcx rustc_hir::Ty<'tcx>>, segment: &'tcx PathSegment, generics: &'tcx Generics) -> Result<Vec<Typ>, VirErr> {
        if let Some(args) = &segment.args {
            if args.bindings.len() > 0 {
                todo!();
            }
            if !matches!(args.parenthesized, GenericArgsParentheses::No) {
                todo!();
            }
        }
        if qualified_self.is_some() {
            assert!(generics.has_self);
        }

        let mut idx = 0;
        let mut self_ty = if generics.has_self {
            Some(qualified_self)
        } else {
            None
        };
        let get_next_segment_arg = &mut || {
            if self_ty.is_some() {
                let s = self_ty.unwrap();
                self_ty = None;
                match s {
                    None => None, // self type exists but is unknown
                    Some(s) => Some(GenericArg::Type(s)),
                }
            } else {
                match &segment.args {
                    None => None,
                    Some(args) => {
                        while idx < args.args.len() && matches!(args.args[idx], GenericArg::Lifetime(_)) {
                            idx += 1
                        }
                        if idx < args.args.len() {
                            idx += 1;
                            Some(args.args[idx - 1])
                        } else {
                            None
                        }
                    }
                }
            }
        };

        let mut v: Vec<Typ> = vec![];
        for generic_param_def in generics.params.iter() {
            match &generic_param_def.kind {
                GenericParamDefKind::Lifetime => { }
                GenericParamDefKind::Type { has_default, synthetic } => {
                    if *has_default { todo!() }
                    if *synthetic { todo!() }

                    let arg = get_next_segment_arg();
                    let typ = match arg {
                        None => self.new_unknown_typ(),
                        Some(GenericArg::Lifetime(_)) => unreachable!(),
                        Some(arg @ GenericArg::Const(_)) => {
                            return err_span(arg.span(), "unexpected const param (normal type param expected)");
                        }
                        Some(GenericArg::Infer(_)) => self.new_unknown_typ(),
                        Some(GenericArg::Type(ty)) => self.check_ty(ty)?,
                    };
                    v.push(typ);
                }
                GenericParamDefKind::Const { has_default, is_host_effect: false } => {
                    if *has_default { todo!() }
                    todo!()
                }
                GenericParamDefKind::Const { has_default: _, is_host_effect: true } => { }
            }
        }

        if let Some(next) = get_next_segment_arg() {
            return err_span(next.span(), "unexpected type param");
        }

        Ok(v)
    }

    pub fn get_item_mode(&self, def_id: DefId) -> Result<vir::ast::Mode, VirErr> {
        match def_id.as_local() {
            Some(local_def_id) => {
                let hir_id = self.tcx.local_def_id_to_hir_id(local_def_id);
                let attrs = self.tcx.hir().attrs(hir_id);
                let mode = crate::attributes::get_mode_opt(attrs);
                match mode {
                    Some(mode) => Ok(mode),
                    None => Ok(vir::ast::Mode::Exec),
                }
            }
            None => {
                todo!()
            }
        }
    }
}

// Implement this trait so we can call probe_generic_path_segments
impl<'a, 'tcx> HirTyLowerer<'tcx> for State<'a, 'tcx> {
    fn tcx(&self) -> TyCtxt<'tcx> { self.tcx }

    fn item_def_id(&self) -> DefId { unreachable!() }

    fn allow_infer(&self) -> bool { unreachable!() }

    fn re_infer(
        &self,
        _param: Option<&GenericParamDef>,
        _span: Span
    ) -> Option<Region<'tcx>> { unreachable!() }

    fn ty_infer(&self, _param: Option<&GenericParamDef>, _span: Span) -> Ty<'tcx> {
        unreachable!()
    }

    fn ct_infer(
        &self,
        _ty: Ty<'tcx>,
        _param: Option<&GenericParamDef>,
        _span: Span
    ) -> Const<'tcx> { unreachable!() }

    fn probe_ty_param_bounds(
        &self,
        _span: Span,
        _def_id: LocalDefId,
        _assoc_name: rustc_span::symbol::Ident
    ) -> GenericPredicates<'tcx> { unreachable!() }

    fn lower_assoc_ty(
        &self,
        _span: Span,
        _item_def_id: DefId,
        _item_segment: &rustc_hir::PathSegment,
        _poly_trait_ref: PolyTraitRef<'tcx>
    ) -> Ty<'tcx> { unreachable!() }

    fn probe_adt(&self, _span: Span, _ty: Ty<'tcx>) -> Option<AdtDef<'tcx>> {
        todo!()
    }

    fn record_ty(&self, _hir_id: HirId, _ty: Ty<'tcx>, _span: Span) {
        unreachable!()
    }

    fn infcx(&self) -> Option<&InferCtxt<'tcx>> {
        unreachable!()
    }

    fn set_tainted_by_errors(&self, _e: ErrorGuaranteed) {
        unreachable!()
    }
}
