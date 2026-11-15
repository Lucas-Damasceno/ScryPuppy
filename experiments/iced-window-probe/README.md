# Prova técnica de janelas Iced

Executável isolado para investigar múltiplas janelas e composição transparente no Windows. Não faz parte da versão distribuída e não acessa dados do usuário.

Execute `cargo run --manifest-path experiments/iced-window-probe/Cargo.toml` a partir da raiz do projeto. Feche a janela principal para encerrar o processo.

No Windows, a prova aplica uma região nativa às áreas dos controles, deixando os espaços vazios fora da janela. A geometria é fixa e não acompanha alterações de DPI ou layout: é apenas uma experiência, não uma implementação pronta para produção.

Esta prova ainda não implementa bandeja, atalhos, posicionamento nem foco sem ativação. A passagem de cliques, as mudanças de DPI e a acessibilidade precisam de validação antes de substituir a aplicação existente.
