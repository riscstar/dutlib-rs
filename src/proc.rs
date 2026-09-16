use std::collections::HashMap;
use std::fmt;
use std::io;
use std::ops::{Index, Sub};

use crate::{CommandExecutor, UnrecoverableError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Irq {
    Irq(u32),
    Ipi(u32),
    Other(String),
}

impl fmt::Display for Irq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Irq::Irq(n) => write!(f, "{}", n),
            Irq::Ipi(n) => write!(f, "IPI{}", n),
            Irq::Other(s) => write!(f, "{}", s),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interrupt {
    pub irq: Irq,
    pub irqchip: String,
    pub hwirq: String,
    pub trigger_mode: String,
    pub name: String,
    pub counts: Vec<u64>,
}

impl Interrupt {
    pub fn count(&self) -> u64 {
        self.counts.iter().sum()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interrupts {
    data: HashMap<String, Interrupt>,
}

impl fmt::Display for Interrupts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut keys: Vec<_> = self.data.keys().collect();
        keys.sort();

        for key in keys {
            let interrupt = &self.data[key];
            write!(f, "{:>4}: ", interrupt.irq)?;
            for count in &interrupt.counts {
                write!(f, "{:>10} ", count)?;
            }
            if !interrupt.irqchip.is_empty() {
                write!(
                    f,
                    " {:<10} {:>10} {:<10} ",
                    interrupt.irqchip, interrupt.hwirq, interrupt.trigger_mode
                )?;
            }
            writeln!(f, " {}", interrupt.name)?;
        }
        Ok(())
    }
}

impl Index<&str> for Interrupts {
    type Output = Interrupt;

    fn index(&self, index: &str) -> &Self::Output {
        &self.data[index]
    }
}

impl Interrupts {
    pub fn new(txt: &str) -> Option<Self> {
        let mut lines = txt.lines();
        let header = lines.next()?;
        let cpu_count = header.split_whitespace().count();
        if cpu_count == 0 {
            return None;
        }

        let mut data = HashMap::new();
        for line in lines {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            if tokens.len() <= cpu_count {
                continue;
            }

            let id = tokens[0].to_string();
            let raw_irq = id.trim_end_matches(':');
            let irq = if let Ok(n) = raw_irq.parse::<u32>() {
                Irq::Irq(n)
            } else if raw_irq.starts_with("IPI") {
                if let Ok(n) = raw_irq[3..].parse::<u32>() {
                    Irq::Ipi(n)
                } else {
                    Irq::Other(raw_irq.to_string())
                }
            } else {
                Irq::Other(raw_irq.to_string())
            };

            let mut counts = Vec::with_capacity(cpu_count);
            for i in 0..cpu_count {
                let count = tokens[i + 1].parse::<u64>().unwrap_or(0);
                counts.push(count);
            }

            let desc_tokens = &tokens[cpu_count + 1..];
            let (irqchip, hwirq, trigger_mode, name) = if desc_tokens.len() >= 4 {
                (
                    desc_tokens[0].to_string(),
                    desc_tokens[1].to_string(),
                    desc_tokens[2].to_string(),
                    desc_tokens[3..].join(" "),
                )
            } else {
                (
                    String::new(),
                    String::new(),
                    String::new(),
                    desc_tokens.join(" "),
                )
            };

            data.insert(
                id,
                Interrupt {
                    irq,
                    irqchip,
                    hwirq,
                    trigger_mode,
                    name,
                    counts,
                },
            );
        }

        Some(Interrupts { data })
    }

    pub fn iter(&self) -> impl Iterator<Item = &Interrupt> {
        self.data.values()
    }

    pub fn filter(&self, s: &str) -> Self {
        let mut data = self.data.clone();
        data.retain(|_, interrupt| interrupt.name.contains(s));
        Interrupts { data }
    }

    pub fn iter_counts(&self) -> impl Iterator<Item = u64> + '_ {
        self.iter().map(|i| i.count())
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }
}

impl Sub for Interrupts {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        &self - &rhs
    }
}

impl<'a, 'b> Sub<&'b Interrupts> for &'a Interrupts {
    type Output = Interrupts;

    fn sub(self, rhs: &'b Interrupts) -> Self::Output {
        let mut data = HashMap::new();
        for (id, lhs_interrupt) in &self.data {
            if let Some(rhs_interrupt) = rhs.data.get(id) {
                let diff = lhs_interrupt
                    .counts
                    .iter()
                    .zip(rhs_interrupt.counts.iter())
                    .map(|(a, b)| a.saturating_sub(*b))
                    .collect();
                data.insert(
                    id.clone(),
                    Interrupt {
                        irq: lhs_interrupt.irq.clone(),
                        irqchip: lhs_interrupt.irqchip.clone(),
                        hwirq: lhs_interrupt.hwirq.clone(),
                        trigger_mode: lhs_interrupt.trigger_mode.clone(),
                        name: lhs_interrupt.name.clone(),
                        counts: diff,
                    },
                );
            } else {
                data.insert(id.clone(), lhs_interrupt.clone());
            }
        }
        Interrupts { data }
    }
}

pub fn interrupts(shell: &mut impl CommandExecutor) -> Result<Interrupts, UnrecoverableError> {
    let output = shell.cmd("cat /proc/interrupts")?;
    Interrupts::new(&output).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Failed to parse /proc/interrupts",
        )
        .into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_INTERRUPTS: &str = "           CPU0       CPU1       CPU2       CPU3       CPU4       CPU5       CPU6       CPU7
  11:  154480992  137518678  115183064  117461341   18639544   22444095   16647619   15629391  GICv3                       27 Level     arch_timer
  13:    6356712          0          0          0          0          0          0          0  GICv3                       37 Level     apps_rsc
  16:   24594312   25655867   21250862   16784330    1938763    1699165    1352173    1278604  GICv3                       38 Level     arch_mem_timer
  17:          0          0          0          0          0          0          0          0  GICv3                       23 Level     arm-pmu
  19:          0          0          0          0          0          0          0          0  msmgpio                    107 Edge      gpio_keys
  20:          0          0          0          0          0          0          0          0  GICv3                       96 Level     arm-smmu global fault";

    #[test]
    fn test_parse_and_count() {
        let interrupts = Interrupts::new(SAMPLE_INTERRUPTS).unwrap();

        // Test Index access and Interrupt::count()
        assert_eq!(interrupts["11:"].count(), 598004724);
        assert_eq!(interrupts["13:"].count(), 6356712);

        // Test multi-word description search via filter
        assert_eq!(
            interrupts
                .filter("arm-smmu global fault")
                .iter_counts()
                .sum::<u64>(),
            0
        );
    }

    #[test]
    fn test_subtraction() {
        let i1 = Interrupts::new(SAMPLE_INTERRUPTS).unwrap();

        // Create a second state with higher values
        let mut i2_txt = SAMPLE_INTERRUPTS.replace("154480992", "154480993"); // Row 11, CPU0 +1
        i2_txt = i2_txt.replace("6356712", "6356722"); // Row 13, CPU0 +10
        let i2 = Interrupts::new(&i2_txt).unwrap();

        let diff = &i2 - &i1;

        assert_eq!(diff["11:"].count(), 1);
        assert_eq!(diff["13:"].count(), 10);
        assert_eq!(diff["17:"].count(), 0);
    }

    #[test]
    fn test_saturating_sub() {
        let i1 = Interrupts::new(SAMPLE_INTERRUPTS).unwrap();
        let i2 = Interrupts::new(SAMPLE_INTERRUPTS).unwrap();

        let diff = &i1 - &i2;
        assert_eq!(diff["11:"].count(), 0);
    }

    #[test]
    fn test_filter() {
        let interrupts = Interrupts::new(SAMPLE_INTERRUPTS).unwrap();

        // Filter to only include "arch_timer"
        let filtered = interrupts.filter("arch_timer");

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered["11:"].name, "arch_timer");
        assert_eq!(filtered["11:"].irqchip, "GICv3");
        assert_eq!(filtered["11:"].hwirq, "27");
        assert_eq!(filtered["11:"].trigger_mode, "Level");
    }

    #[test]
    fn test_iter_counts() {
        let interrupts = Interrupts::new(SAMPLE_INTERRUPTS).unwrap();
        let filtered = interrupts.filter("arch_timer");

        // arch_timer sum was 598004724
        let counts: Vec<u64> = filtered.iter_counts().collect();
        assert_eq!(counts, vec![598004724]);
    }

    #[test]
    fn test_len() {
        let interrupts = Interrupts::new(SAMPLE_INTERRUPTS).unwrap();
        assert_eq!(interrupts.len(), 6);

        let filtered = interrupts.filter("arch_timer");
        assert_eq!(filtered.len(), 1);
    }
}
