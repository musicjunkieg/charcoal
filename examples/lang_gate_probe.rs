//! Probe: does the ONNX clean-pass gate discriminate on non-Latin text? (#222)
//!
//! `TwoStageToxicityScorer::classify_post` short-circuits to `is_toxic: false`
//! whenever the English-trained Detoxify model scores below `ONNX_CLEAN_THRESHOLD`
//! (0.10) — Stage 2 is never called. Since CoPE-B is itself English-only, a post
//! the ONNX gate clears is never assessed by anything.
//!
//! The question this answers is NOT "what is the true toxicity of this Thai
//! sentence" — it's "can the gate tell hostile and benign apart at all in this
//! script". That framing is deliberate: it does not require certifying any
//! individual string's toxicity. If hostile and benign land in the same narrow
//! band, the gate has no signal and the comparison stands on its own.
//!
//! Samples are parallel translations — each group says the same five things —
//! so cross-language differences aren't confounded by differing content.
//!
//! The English groups are CONTROLS, and they are load-bearing: if en_hostile
//! does not score high, the harness is broken and no conclusion about any other
//! group is valid. A clean result only counts if a dirty one was possible.
//!
//! Run: cargo run --release --example lang_gate_probe

use anyhow::Result;
use charcoal::toxicity::onnx::OnnxToxicityScorer;
use charcoal::toxicity::traits::ToxicityScorer;
use std::path::Path;

/// Mirrors `charcoal::toxicity::ensemble::ONNX_CLEAN_THRESHOLD`.
const GATE: f64 = 0.10;

struct Group {
    name: &'static str,
    /// True when these samples SHOULD trip the gate (hostile content).
    hostile: bool,
    samples: &'static [&'static str],
}

/// Five parallel statements, rendered in each language.
///
/// Hostile set: 1) generic insult + die, 2) worthless trash, 3) fat-shaming,
/// 4) hope you die, 5) nobody wants you. Benign set: 1) nice weather,
/// 2) thanks/compliment, 3) restaurant recommendation, 4) meeting tomorrow,
/// 5) birthday wishes.
///
/// NOTE: the non-English strings were authored by Claude and have NOT been
/// checked by a native speaker. See the caveat printed at the end of the run.
const GROUPS: &[Group] = &[
    Group {
        name: "en_hostile (POSITIVE CONTROL — must score HIGH)",
        hostile: true,
        samples: &[
            "You're a fucking idiot, go kill yourself",
            "Shut up, you worthless piece of trash",
            "Fat people like you are disgusting and should be ashamed",
            "I hope you die, you stupid bitch",
            "Nobody wants you here, you pathetic loser",
        ],
    },
    Group {
        name: "en_benign  (NEGATIVE CONTROL — must score LOW)",
        hostile: false,
        samples: &[
            "The weather is lovely today, let's go for a walk",
            "Thank you so much, I really like your work",
            "The food at this restaurant is delicious, highly recommend",
            "I have a meeting tomorrow morning",
            "Happy birthday! Hope you have a wonderful day",
        ],
    },
    Group {
        name: "th_hostile (Thai — the measured population)",
        hostile: true,
        samples: &[
            "แกมันโง่ชิบหาย ไปตายซะ",
            "หุบปากไปเลย ไอ้ขยะไร้ค่า",
            "คนอ้วนแบบมึงน่าขยะแขยง ควรจะอาย",
            "กูหวังว่ามึงจะตาย ไอ้โง่",
            "ไม่มีใครต้องการมึงที่นี่ ไอ้ขี้แพ้",
        ],
    },
    Group {
        name: "th_benign  (Thai)",
        hostile: false,
        samples: &[
            "วันนี้อากาศดีมาก ไปเดินเล่นกันเถอะ",
            "ขอบคุณมากครับ ผมชอบงานของคุณมาก",
            "อาหารร้านนี้อร่อยมาก แนะนำเลยครับ",
            "พรุ่งนี้เช้าผมมีประชุม",
            "สุขสันต์วันเกิดนะ ขอให้มีความสุขมากๆ",
        ],
    },
    Group {
        name: "ja_hostile (Japanese — is this script-general?)",
        hostile: true,
        samples: &[
            "お前は本当に馬鹿だ、死ね",
            "黙れ、この価値のないゴミが",
            "お前みたいなデブは気持ち悪い、恥を知れ",
            "お前が死ぬことを願う、この馬鹿",
            "誰もお前をここで必要としていない、負け犬",
        ],
    },
    Group {
        name: "ru_hostile (Cyrillic — is this script-general?)",
        hostile: true,
        samples: &[
            "Ты чёртов идиот, иди убей себя",
            "Заткнись, ты бесполезный мусор",
            "Толстые люди вроде тебя отвратительны, стыдись",
            "Надеюсь, ты сдохнешь, тупая сука",
            "Ты здесь никому не нужен, жалкий неудачник",
        ],
    },
    // Latin-script non-English. Script-based detection CANNOT distinguish these
    // from English, so if the model fails here too, a script heuristic is not a
    // sufficient abstention trigger and we need real language identification.
    // Portuguese matters most in practice — Bluesky has a large Brazilian user
    // base — so it is the one to watch.
    Group {
        name: "pt_hostile (Portuguese — Latin script, not English)",
        hostile: true,
        samples: &[
            "Você é um idiota do caralho, vai se matar",
            "Cala a boca, seu lixo inútil",
            "Gordos como você são nojentos, devia ter vergonha",
            "Espero que você morra, sua vadia burra",
            "Ninguém te quer aqui, seu perdedor patético",
        ],
    },
    Group {
        name: "pt_benign  (Portuguese)",
        hostile: false,
        samples: &[
            "O tempo está lindo hoje, vamos dar uma caminhada",
            "Muito obrigado, eu gosto muito do seu trabalho",
            "A comida deste restaurante é deliciosa, recomendo muito",
            "Tenho uma reunião amanhã de manhã",
            "Feliz aniversário! Espero que tenha um dia maravilhoso",
        ],
    },
    Group {
        name: "de_hostile (German — Latin script, not English)",
        hostile: true,
        samples: &[
            "Du bist ein verdammter Idiot, bring dich um",
            "Halt die Fresse, du wertloser Müll",
            "Dicke Menschen wie du sind widerlich, schäm dich",
            "Ich hoffe, du stirbst, du dumme Schlampe",
            "Niemand will dich hier, du erbärmlicher Versager",
        ],
    },
    // Confound control. If the non-Latin samples score low because the model
    // READ them and judged them benign, meaningless character salad should
    // score differently from real sentences. If instead every non-Latin input
    // lands in the same narrow band as this row, the model is emitting a floor
    // constant for unfamiliar script — it has no signal, rather than a wrong
    // opinion. This also covers the "maybe Claude's Thai was just bad" reading:
    // bad Thai and good Thai would both be indistinguishable from noise.
    Group {
        name: "xx_gibberish (CONFOUND CONTROL — non-Latin noise)",
        hostile: false,
        samples: &[
            "ณฤฒฆษฑ ฎฏโใฬ ฝฃฐฉ",
            "぀ゕヸ㍿龘齾鼳 ㋛㌇",
            "ъыьэюя ёђѓєѕіїј ћќѝўџ",
            "ᚠᚡᚢᚣᚤᚥ ᚦᚧᚨᚩ ᚪᚫᚬ",
            "๗๘๙๐ ฿๚๛ ฃฅฎฏ",
        ],
    },
];

#[tokio::main]
async fn main() -> Result<()> {
    let scorer = OnnxToxicityScorer::load(Path::new("models"))?;

    println!("ONNX clean-pass gate probe (#222)");
    println!("Gate: scores below {GATE:.2} short-circuit to is_toxic=false, Stage 2 never runs.\n");

    let mut summaries = Vec::new();

    for group in GROUPS {
        println!("{}", group.name);
        let mut scores = Vec::with_capacity(group.samples.len());

        for text in group.samples {
            let result = scorer.score_text(text).await?;
            let score = result.toxicity;
            // "CLEARED" means the gate declared it non-toxic with no further
            // assessment. For a hostile sample that is a silent false negative.
            let verdict = if score < GATE {
                "CLEARED"
            } else {
                "-> stage 2"
            };
            println!("  {score:>7.4}  {verdict:<10}  {text}");
            scores.push(score);
        }

        let n = scores.len() as f64;
        let mean = scores.iter().sum::<f64>() / n;
        let max = scores.iter().cloned().fold(f64::MIN, f64::max);
        let cleared = scores.iter().filter(|s| **s < GATE).count();
        println!(
            "  --- mean {mean:.4}  max {max:.4}  cleared {cleared}/{}\n",
            scores.len()
        );

        summaries.push((group.name, group.hostile, mean, max, cleared, scores.len()));
    }

    println!("SUMMARY");
    println!(
        "  {:<52} {:>8} {:>8} {:>10}",
        "group", "mean", "max", "cleared"
    );
    for (name, _, mean, max, cleared, total) in &summaries {
        println!(
            "  {name:<52} {mean:>8.4} {max:>8.4} {:>10}",
            format!("{cleared}/{total}")
        );
    }

    // Control check — stated explicitly so a broken harness cannot be read as
    // a finding about language coverage.
    let pos = summaries
        .iter()
        .find(|s| s.0.starts_with("en_hostile"))
        .unwrap();
    let neg = summaries
        .iter()
        .find(|s| s.0.starts_with("en_benign"))
        .unwrap();
    println!("\nCONTROL CHECK");
    if pos.4 == 0 && neg.4 == neg.5 {
        println!("  PASS — English hostile all reached stage 2, English benign all cleared.");
        println!("  The harness can produce both outcomes, so the other rows are meaningful.");
    } else {
        println!("  FAIL — controls did not behave as expected (hostile cleared {}/{}, benign cleared {}/{}).",
            pos.4, pos.5, neg.4, neg.5);
        println!("  DO NOT draw conclusions about the non-English rows from this run.");
    }

    println!("\nCAVEAT: the non-English samples were authored by Claude and have not been");
    println!("verified by a native speaker. The hostile-vs-benign CONTRAST within a language");
    println!("is the load-bearing comparison, not any single absolute score.");

    Ok(())
}
