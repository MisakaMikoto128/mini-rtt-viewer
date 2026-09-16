//! 一次性语义探针:验证 Slint 表达式 `floor(length / length) * 1px` 的实际求值,
//! 以及 floor 对负数的行为。用完即删,不进发布物。
slint::slint! {
    export component Probe inherits Window {
        out property <float> ratio: 9200px / 19px;
        out property <length> quant: floor(9200px / 19px) * 1px;
        out property <length> small: floor(130px / 19px) * 1px;
        out property <float> negf: floor(-6.8);
        out property <length> negq: floor(-130px / 19px) * 1px;
        out property <length> direct: 9200px;
    }
}

fn main() {
    let p = Probe::new().unwrap();
    println!("ratio  (9200px / 19px)          = {}", p.get_ratio());
    println!("quant  floor(9200px/19px)*1px   = {:?}", p.get_quant());
    println!("small  floor(130px/19px)*1px    = {:?}", p.get_small());
    println!("negf   floor(-6.8)              = {}", p.get_negf());
    println!("negq   floor(-130px/19px)*1px   = {:?}", p.get_negq());
    println!("direct 9200px                  = {:?}", p.get_direct());
}
