// Policy gradient example.
// This uses OpenAI Gym environment through rust-cpython.
// For now this uses the CartPole-v0 environment and hardcodes the number
// of observations (4) and actions (2).
//
// This is adapted from OpenAI Spinning Up series:
// https://spinningup.openai.com/en/latest/spinningup/rl_intro3.html
// A TensorFlow Python implementation can be found here:
// https://github.com/openai/spinningup/blob/master/spinup/examples/pg_math/2_rtg_pg.py
extern crate cpython;
extern crate tch;

use cpython::{NoArgs, ObjectProtocol, PyObject, PyResult, Python};
use tch::nn::OptimizerConfig;
use tch::{nn, Tensor};

fn model(p: &nn::Path) -> impl nn::Module {
    nn::Sequential::new()
        .add(nn::Linear::new(p / "lin1", 4, 32, Default::default()))
        .add_fn(|xs| xs.tanh())
        .add(nn::Linear::new(p / "lin2", 32, 2, Default::default()))
}

#[derive(Debug)]
struct Step {
    obs: Tensor,
    action: i64,
    reward: f64,
    is_done: bool,
}

impl Step {
    fn copy_with_obs(&self, obs: &Tensor) -> Step {
        Step {
            obs: obs.copy(),
            action: self.action,
            reward: self.reward,
            is_done: self.is_done,
        }
    }
}

fn accumulate_rewards(steps: &[Step]) -> Vec<f64> {
    let mut rewards: Vec<f64> = steps.iter().map(|s| s.reward).collect();
    let mut acc_reward = 0f64;
    for (i, reward) in rewards.iter_mut().enumerate().rev() {
        if steps[i].is_done {
            acc_reward = 0.0;
        }
        acc_reward += *reward;
        *reward = acc_reward;
    }
    rewards
}

struct GymEnv<'a> {
    py: Python<'a>,
    env: PyObject,
}

impl<'a> GymEnv<'a> {
    fn new(gil: &cpython::GILGuard) -> PyResult<GymEnv> {
        let py = gil.python();
        let gym = py.import("gym")?;
        let env = gym.call(py, "make", ("CartPole-v0",), None)?;
        let _ = env.call_method(py, "seed", (42,), None)?;
        Ok(GymEnv { py, env })
    }

    fn reset(&self) -> PyResult<Tensor> {
        let obs = self.env.call_method(self.py, "reset", NoArgs, None)?;
        Ok(Tensor::float_vec(&obs.extract::<Vec<f64>>(self.py)?))
    }

    fn step(&self, action: i64) -> PyResult<Step> {
        let py = self.py;
        let step = self.env.call_method(py, "step", (action,), None)?;
        Ok(Step {
            obs: Tensor::float_vec(&step.get_item(py, 0)?.extract::<Vec<f64>>(py)?),
            reward: step.get_item(py, 1)?.extract(py)?,
            is_done: step.get_item(py, 2)?.extract(py)?,
            action,
        })
    }
}

fn main() -> PyResult<()> {
    let vs = nn::VarStore::new(tch::Device::Cpu);
    let model = model(&vs.root());
    let opt = nn::Adam::default().build(&vs, 1e-2).unwrap();

    let gil = Python::acquire_gil();
    let env = GymEnv::new(&gil)?;

    for epoch_idx in 0..50 {
        let mut obs = env.reset()?;
        let mut steps: Vec<Step> = vec![];
        // Perform some rollouts with the current model.
        loop {
            let action = tch::no_grad(|| {
                obs.unsqueeze(0)
                    .apply(&model)
                    .softmax(1)
                    .multinomial(1, true)
            });
            let action = i64::from(action);
            let step = env.step(action)?;
            steps.push(step.copy_with_obs(&obs));
            obs = if step.is_done { env.reset()? } else { step.obs };
            if step.is_done && steps.len() > 5000 {
                break;
            }
        }
        let sum_r: f64 = steps.iter().map(|s| s.reward).sum();
        let episodes: i64 = steps.iter().map(|s| s.is_done as i64).sum();
        println!(
            "epoch: {:<3} episodes: {:<5} avg reward per episode: {:.2}",
            epoch_idx,
            episodes,
            sum_r / episodes as f64
        );

        // Train the model via policy gradient on the rollout data.
        let batch_size = steps.len() as i64;
        let actions: Vec<i64> = steps.iter().map(|s| s.action).collect();
        let actions = Tensor::int_vec(&actions).unsqueeze(1);
        let rewards = accumulate_rewards(&steps);
        let rewards = Tensor::float_vec(&rewards);
        let action_mask = Tensor::zeros(&[batch_size, 2], tch::kind::FLOAT_CPU).scatter_(
            1,
            &actions,
            &Tensor::from(1.),
        );
        let obs: Vec<Tensor> = steps.into_iter().map(|s| s.obs).collect();
        let logits = Tensor::stack(&obs, 0).apply(&model);
        let log_probs = (action_mask * logits.log_softmax(1)).sum2(&[1], false);
        let loss = -(rewards * log_probs).mean();
        opt.backward_step(&loss)
    }
    Ok(())
}
