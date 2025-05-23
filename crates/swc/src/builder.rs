use std::sync::Arc;

use compat::es2015::regenerator;
use either::Either;
use rustc_hash::FxHashMap;
use swc_atoms::Atom;
use swc_common::{
    comments::Comments, errors::Handler, sync::Lrc, util::take::Take, FileName, Mark, SourceMap,
};
use swc_ecma_ast::{EsVersion, Module, Pass, Script};
use swc_ecma_minifier::option::{terser::TerserTopLevelOptions, MinifyOptions};
use swc_ecma_parser::Syntax;
use swc_ecma_preset_env::Caniuse;
use swc_ecma_transforms::{
    compat,
    fixer::{fixer, paren_remover},
    helpers,
    hygiene::{self, hygiene_with_config},
    modules::{self, path::ImportResolver},
    optimization::const_modules,
    resolver, Assumptions,
};
use swc_ecma_visit::{noop_visit_mut_type, visit_mut_pass, VisitMut, VisitMutWith};
use swc_visit::Optional;

use crate::config::{GlobalPassOption, JsMinifyOptions, ModuleConfig};

/// Builder is used to create a high performance `Compiler`.
pub struct PassBuilder<'a, 'b, P: Pass> {
    cm: &'a Arc<SourceMap>,
    handler: &'b Handler,
    env: Option<swc_ecma_preset_env::EnvConfig>,
    pass: P,
    /// [Mark] for top level bindings .
    top_level_mark: Mark,

    /// [Mark] for unresolved refernces.
    unresolved_mark: Mark,

    target: EsVersion,
    loose: bool,
    assumptions: Assumptions,
    hygiene: Option<hygiene::Config>,
    fixer: bool,
    inject_helpers: bool,
    minify: Option<JsMinifyOptions>,
    regenerator: regenerator::Config,
}

impl<'a, 'b, P: Pass> PassBuilder<'a, 'b, P> {
    pub fn new(
        cm: &'a Arc<SourceMap>,
        handler: &'b Handler,
        loose: bool,
        assumptions: Assumptions,
        top_level_mark: Mark,
        unresolved_mark: Mark,
        pass: P,
    ) -> Self {
        PassBuilder {
            cm,
            handler,
            env: None,
            pass,
            top_level_mark,
            unresolved_mark,
            target: EsVersion::Es5,
            loose,
            assumptions,
            hygiene: Some(Default::default()),
            fixer: true,
            inject_helpers: true,
            minify: None,
            regenerator: Default::default(),
        }
    }

    pub fn then<N>(self, next: N) -> PassBuilder<'a, 'b, (P, N)>
    where
        N: Pass,
    {
        let pass = (self.pass, next);
        PassBuilder {
            cm: self.cm,
            handler: self.handler,
            env: self.env,
            pass,
            top_level_mark: self.top_level_mark,
            unresolved_mark: self.unresolved_mark,
            target: self.target,
            loose: self.loose,
            assumptions: self.assumptions,
            hygiene: self.hygiene,
            fixer: self.fixer,
            inject_helpers: self.inject_helpers,
            minify: self.minify,
            regenerator: self.regenerator,
        }
    }

    pub fn skip_helper_injection(mut self, skip: bool) -> Self {
        self.inject_helpers = !skip;
        self
    }

    pub fn minify(mut self, options: Option<JsMinifyOptions>) -> Self {
        self.minify = options;
        self
    }

    /// Note: fixer is enabled by default.
    pub fn fixer(mut self, enable: bool) -> Self {
        self.fixer = enable;
        self
    }

    /// Note: hygiene is enabled by default.
    ///
    /// If you pass [None] to this method, the `hygiene` pass will be disabled.
    pub fn hygiene(mut self, config: Option<hygiene::Config>) -> Self {
        self.hygiene = config;
        self
    }

    pub fn const_modules(
        self,
        globals: FxHashMap<Atom, FxHashMap<Atom, String>>,
    ) -> PassBuilder<'a, 'b, (P, impl Pass)> {
        let cm = self.cm.clone();
        self.then(const_modules(cm, globals))
    }

    pub fn inline_globals(self, c: GlobalPassOption) -> PassBuilder<'a, 'b, (P, impl Pass)> {
        let pass = c.build(self.cm, self.handler, self.unresolved_mark);
        self.then(pass)
    }

    pub fn target(mut self, target: EsVersion) -> Self {
        self.target = target;
        self
    }

    pub fn preset_env(mut self, env: Option<swc_ecma_preset_env::Config>) -> Self {
        self.env = env.map(Into::into);
        self
    }

    pub fn regenerator(mut self, config: regenerator::Config) -> Self {
        self.regenerator = config;
        self
    }

    /// # Arguments
    /// ## module
    ///  - Use `None` if you want swc to emit import statements.
    ///
    ///
    /// Returned pass includes
    ///
    ///  - compatibility helper
    ///  - module handler
    ///  - helper injector
    ///  - identifier hygiene handler if enabled
    ///  - fixer if enabled
    pub fn finalize<'cmt>(
        self,
        _syntax: Syntax,
        module: Option<ModuleConfig>,
        comments: Option<&'cmt dyn Comments>,
        resolver: Option<(FileName, Arc<dyn ImportResolver>)>,
    ) -> impl 'cmt + Pass
    where
        P: 'cmt,
    {
        let (need_analyzer, import_interop, ignore_dynamic) = match module {
            Some(ModuleConfig::CommonJs(ref c)) => (true, c.import_interop(), c.ignore_dynamic),
            Some(ModuleConfig::Amd(ref c)) => {
                (true, c.config.import_interop(), c.config.ignore_dynamic)
            }
            Some(ModuleConfig::Umd(ref c)) => {
                (true, c.config.import_interop(), c.config.ignore_dynamic)
            }
            Some(ModuleConfig::SystemJs(_))
            | Some(ModuleConfig::Es6(..))
            | Some(ModuleConfig::NodeNext(..))
            | None => (false, true.into(), true),
        };

        let feature_config = self.env.as_ref().map(|e| e.get_feature_config());

        // compat
        let compat_pass = {
            if let Some(env_config) = self.env {
                Either::Left(swc_ecma_preset_env::transform_from_env(
                    self.unresolved_mark,
                    comments,
                    env_config,
                    self.assumptions,
                ))
            } else {
                Either::Right(swc_ecma_preset_env::transform_from_es_version(
                    self.unresolved_mark,
                    comments,
                    self.target,
                    self.assumptions,
                    self.loose,
                ))
            }
        };

        let is_mangler_enabled = self
            .minify
            .as_ref()
            .map(|v| v.mangle.is_obj() || v.mangle.is_true())
            .unwrap_or(false);

        (
            self.pass,
            Optional::new(
                paren_remover(comments.map(|v| v as &dyn Comments)),
                self.fixer,
            ),
            compat_pass,
            // module / helper
            Optional::new(
                modules::import_analysis::import_analyzer(import_interop, ignore_dynamic),
                need_analyzer,
            ),
            Optional::new(
                helpers::inject_helpers(self.unresolved_mark),
                self.inject_helpers,
            ),
            ModuleConfig::build(
                self.cm.clone(),
                comments,
                module,
                self.unresolved_mark,
                resolver,
                |f| {
                    feature_config
                        .as_ref()
                        .map_or_else(|| self.target.caniuse(f), |env| env.caniuse(f))
                },
            ),
            visit_mut_pass(MinifierPass {
                options: self.minify,
                cm: self.cm.clone(),
                comments,
                top_level_mark: self.top_level_mark,
            }),
            Optional::new(
                hygiene_with_config(swc_ecma_transforms_base::hygiene::Config {
                    top_level_mark: self.top_level_mark,
                    ..self.hygiene.clone().unwrap_or_default()
                }),
                self.hygiene.is_some() && !is_mangler_enabled,
            ),
            Optional::new(fixer(comments.map(|v| v as &dyn Comments)), self.fixer),
        )
    }
}

struct MinifierPass<'a> {
    options: Option<JsMinifyOptions>,
    cm: Lrc<SourceMap>,
    comments: Option<&'a dyn Comments>,
    top_level_mark: Mark,
}

impl VisitMut for MinifierPass<'_> {
    noop_visit_mut_type!(fail);

    fn visit_mut_module(&mut self, m: &mut Module) {
        if let Some(options) = &self.options {
            let opts = MinifyOptions {
                compress: options
                    .compress
                    .clone()
                    .unwrap_as_option(|default| match default {
                        Some(true) => Some(Default::default()),
                        _ => None,
                    })
                    .map(|mut v| {
                        if v.const_to_let.is_none() {
                            v.const_to_let = Some(true);
                        }
                        if v.toplevel.is_none() {
                            v.toplevel = Some(TerserTopLevelOptions::Bool(true));
                        }

                        v.into_config(self.cm.clone())
                    }),
                mangle: options
                    .mangle
                    .clone()
                    .unwrap_as_option(|default| match default {
                        Some(true) => Some(Default::default()),
                        _ => None,
                    }),
                ..Default::default()
            };

            if opts.compress.is_none() && opts.mangle.is_none() {
                return;
            }

            m.visit_mut_with(&mut hygiene_with_config(
                swc_ecma_transforms_base::hygiene::Config {
                    top_level_mark: self.top_level_mark,
                    ..Default::default()
                },
            ));

            let unresolved_mark = Mark::new();
            let top_level_mark = Mark::new();

            m.visit_mut_with(&mut resolver(unresolved_mark, top_level_mark, false));

            m.map_with_mut(|m| {
                swc_ecma_minifier::optimize(
                    m.into(),
                    self.cm.clone(),
                    self.comments.as_ref().map(|v| v as &dyn Comments),
                    None,
                    &opts,
                    &swc_ecma_minifier::option::ExtraOptions {
                        unresolved_mark,
                        top_level_mark,
                        mangle_name_cache: None,
                    },
                )
                .expect_module()
            })
        }
    }

    fn visit_mut_script(&mut self, m: &mut Script) {
        if let Some(options) = &self.options {
            let opts = MinifyOptions {
                compress: options
                    .compress
                    .clone()
                    .unwrap_as_option(|default| match default {
                        Some(true) => Some(Default::default()),
                        _ => None,
                    })
                    .map(|mut v| {
                        if v.const_to_let.is_none() {
                            v.const_to_let = Some(true);
                        }

                        v.module = false;

                        v.into_config(self.cm.clone())
                    }),
                mangle: options
                    .mangle
                    .clone()
                    .unwrap_as_option(|default| match default {
                        Some(true) => Some(Default::default()),
                        _ => None,
                    }),
                ..Default::default()
            };

            if opts.compress.is_none() && opts.mangle.is_none() {
                return;
            }

            m.visit_mut_with(&mut hygiene_with_config(
                swc_ecma_transforms_base::hygiene::Config {
                    top_level_mark: self.top_level_mark,
                    ..Default::default()
                },
            ));

            let unresolved_mark = Mark::new();
            let top_level_mark = Mark::new();

            m.visit_mut_with(&mut resolver(unresolved_mark, top_level_mark, false));

            m.map_with_mut(|m| {
                swc_ecma_minifier::optimize(
                    m.into(),
                    self.cm.clone(),
                    self.comments.as_ref().map(|v| v as &dyn Comments),
                    None,
                    &opts,
                    &swc_ecma_minifier::option::ExtraOptions {
                        unresolved_mark,
                        top_level_mark,
                        mangle_name_cache: None,
                    },
                )
                .expect_script()
            })
        }
    }
}
