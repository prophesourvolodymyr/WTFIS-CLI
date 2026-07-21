class Wtfis < Formula
  desc "Find projects fast from your terminal"
  homepage "https://github.com/prophesourvolodymyr/WTFIS-CLI"
  url "https://github.com/prophesourvolodymyr/WTFIS-CLI/archive/refs/tags/v0.1.0.tar.gz"
  version "0.1.0"
  license "MIT"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: "Cargo.toml")
    bin.install_symlink "wtfis" => "cdd"
    pkgshare.install "shell/wtfis.zsh", "shell/wtfis.bash"
  end

  test do
    assert_match "wtfis", shell_output("#{bin}/wtfis --help 2>&1", 0)
  end
end
