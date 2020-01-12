use crate::solve::slg::SlgContext;
use crate::RustIrDatabase;
use chalk_engine::forest::{Forest, SubstitutionResult};
use chalk_ir::family::TypeFamily;
use chalk_ir::*;
use std::fmt;

mod slg;
mod truncate;

/// A (possible) solution for a proposed goal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Solution<TF: TypeFamily> {
    /// The goal indeed holds, and there is a unique value for all existential
    /// variables. In this case, we also record a set of lifetime constraints
    /// which must also hold for the goal to be valid.
    Unique(Canonical<ConstrainedSubst<TF>>),

    /// The goal may be provable in multiple ways, but regardless we may have some guidance
    /// for type inference. In this case, we don't return any lifetime
    /// constraints, since we have not "committed" to any particular solution
    /// yet.
    Ambig(Guidance<TF>),
}

/// When a goal holds ambiguously (e.g., because there are multiple possible
/// solutions), we issue a set of *guidance* back to type inference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Guidance<TF: TypeFamily> {
    /// The existential variables *must* have the given values if the goal is
    /// ever to hold, but that alone isn't enough to guarantee the goal will
    /// actually hold.
    Definite(Canonical<Substitution<TF>>),

    /// There are multiple plausible values for the existentials, but the ones
    /// here are suggested as the preferred choice heuristically. These should
    /// be used for inference fallback only.
    Suggested(Canonical<Substitution<TF>>),

    /// There's no useful information to feed back to type inference
    Unknown,
}

impl<TF: TypeFamily> Solution<TF> {
    pub fn is_unique(&self) -> bool {
        match *self {
            Solution::Unique(..) => true,
            _ => false,
        }
    }
}

impl<TF: TypeFamily> fmt::Display for Solution<TF> {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        match self {
            Solution::Unique(constrained) => write!(f, "Unique; {}", constrained,),
            Solution::Ambig(Guidance::Definite(subst)) => {
                write!(f, "Ambiguous; definite substitution {}", subst)
            }
            Solution::Ambig(Guidance::Suggested(subst)) => {
                write!(f, "Ambiguous; suggested substitution {}", subst)
            }
            Solution::Ambig(Guidance::Unknown) => write!(f, "Ambiguous; no inference guidance"),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialOrd, Ord, PartialEq, Eq, Hash)]
pub enum SolverChoice {
    /// Run the SLG solver, producing a Solution.
    SLG {
        max_size: usize,
        expected_answers: Option<usize>,
    },
}

impl SolverChoice {
    /// Returns the default SLG parameters.
    pub fn slg(max_size: usize, expected_answers: Option<usize>) -> Self {
        SolverChoice::SLG {
            max_size,
            expected_answers,
        }
    }

    /// Creates a solver state.
    pub fn into_solver<TF: TypeFamily>(self) -> Solver<TF> {
        match self {
            SolverChoice::SLG {
                max_size,
                expected_answers,
            } => Solver {
                forest: Forest::new(SlgContext::new(max_size, expected_answers)),
            },
        }
    }
}

impl Default for SolverChoice {
    fn default() -> Self {
        SolverChoice::slg(10, None)
    }
}

/// Finds the solution to "goals", or trait queries -- i.e., figures
/// out what sets of types implement which traits. Also, between
/// queries, this struct stores the cached state from previous solver
/// attempts, which can then be re-used later.
pub struct Solver<TF: TypeFamily> {
    forest: Forest<SlgContext<TF>>,
}

impl<TF: TypeFamily> Solver<TF> {
    /// Attempts to solve the given goal, which must be in canonical
    /// form. Returns a unique solution (if one exists).  This will do
    /// only as much work towards `goal` as it has to (and that work
    /// is cached for future attempts).
    ///
    /// # Parameters
    ///
    /// - `program` -- defines the program clauses in scope.
    ///   - **Important:** You must supply the same set of program clauses
    ///     each time you invoke `solve`, as otherwise the cached data may be
    ///     invalid.
    /// - `goal` the goal to solve
    ///
    /// # Returns
    ///
    /// - `None` is the goal cannot be proven.
    /// - `Some(solution)` if we succeeded in finding *some* answers,
    ///   although `solution` may reflect ambiguity and unknowns.
    pub fn solve(
        &mut self,
        program: &dyn RustIrDatabase<TF>,
        goal: &UCanonical<InEnvironment<Goal<TF>>>,
    ) -> Option<Solution<TF>> {
        let ops = self.forest.context().ops(program);
        self.forest.solve(&ops, goal, || true)
    }

    /// Attempts to solve the given goal, which must be in canonical
    /// form. Returns a unique solution (if one exists).  This will do
    /// only as much work towards `goal` as it has to (and that work
    /// is cached for future attempts). In addition, the solving of the
    /// goal can be limited by returning `false` from `should_continue`.
    ///
    /// # Parameters
    ///
    /// - `program` -- defines the program clauses in scope.
    ///   - **Important:** You must supply the same set of program clauses
    ///     each time you invoke `solve`, as otherwise the cached data may be
    ///     invalid.
    /// - `goal` the goal to solve
    /// - `should_continue` if `false` is returned, the no further solving will be done
    ///
    /// # Returns
    ///
    /// - `None` is the goal cannot be proven.
    /// - `Some(solution)` if we succeeded in finding *some* answers,
    ///   although `solution` may reflect ambiguity and unknowns.
    pub fn solve_limited(
        &mut self,
        program: &dyn RustIrDatabase<TF>,
        goal: &UCanonical<InEnvironment<Goal<TF>>>,
        should_continue: impl std::ops::Fn() -> bool,
    ) -> Option<Solution<TF>> {
        let ops = self.forest.context().ops(program);
        self.forest.solve(&ops, goal, should_continue)
    }

    /// Attempts to solve the given goal, which must be in canonical
    /// form. Provides multiple solutions to function `f`.  This will do
    /// only as much work towards `goal` as it has to (and that work
    /// is cached for future attempts).
    ///
    /// # Parameters
    ///
    /// - `program` -- defines the program clauses in scope.
    ///   - **Important:** You must supply the same set of program clauses
    ///     each time you invoke `solve`, as otherwise the cached data may be
    ///     invalid.
    /// - `goal` the goal to solve
    /// - `f` -- function to proceed solution. New solutions will be generated
    /// while function returns `true`.
    ///   - first argument is solution found
    ///   - second argument is ther next solution present
    ///   - returns true if next solution should be handled
    ///
    /// # Returns
    ///
    /// - `true` all solutions were processed with the function.
    /// - `false` the function returned `false` and solutions were interrupted.
    pub fn solve_multiple(
        &mut self,
        program: &dyn RustIrDatabase<TF>,
        goal: &UCanonical<InEnvironment<Goal<TF>>>,
        f: impl FnMut(SubstitutionResult<Canonical<ConstrainedSubst<TF>>>, bool) -> bool,
    ) -> bool {
        let ops = self.forest.context().ops(program);
        self.forest.solve_multiple(&ops, goal, f)
    }
}

impl<TF: TypeFamily> std::fmt::Debug for Solver<TF> {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(fmt, "Solver {{ .. }}")
    }
}
